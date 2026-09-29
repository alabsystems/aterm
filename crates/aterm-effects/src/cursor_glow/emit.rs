// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EMISSION — turning the engine's live state into this frame's quads,
//! halos and patches, style by style.

use super::*;

/// Test-only counter of `emit_custom` invocations — the BUILTIN-BYTE-IDENTICAL
/// proof asserts it stays 0 across every built-in tick run (no built-in path
/// ever enters the custom interpreter).
#[cfg(test)]
pub(super) static CUSTOM_EMIT_CALLS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

impl CursorGlow {
    // ----- emitting -----

    /// Emit the live LIGHTNING bolts: each frozen jagged channel draws as a wide
    /// same-hue CORONA under a hot core (branches: thinner, dimmer, no white
    /// mix), rasterized through the shared [`comet_beam`] so strikes stay smooth
    /// at any angle and CPU/GPU byte parity holds. Brightness runs a STROBE
    /// envelope — sharp attack, hard dip, RESTRIKE, cosine collapse — the
    /// double-strike flicker that reads as lightning, not a fading streak.
    pub(super) fn emit_bolts(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
        verts: &mut Vec<BeamVertex>,
    ) {
        if !matches!(cfg.style, GlowStyle::Laser) || self.bolts.is_empty() {
            return;
        }
        let ch = geom.ch as f32;
        // Newest strikes first, so a saturated budget sheds the oldest light.
        for b in self.bolts.iter().rev() {
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            let u = (now.saturating_duration_since(b.born).as_secs_f32() / b.life).clamp(0.0, 1.0);
            // Double-strike strobe (attack → dip → restrike → burn-out) with a
            // fast per-bolt shimmer on top so parallel strikes never sync up.
            // The final phase only dims GENTLY — the spatial retraction below is
            // what extinguishes the strike, and it needs light left to retract.
            let base = if u < 0.32 {
                1.0 - 0.55 * (u / 0.32)
            } else if u < 0.42 {
                0.30
            } else if u < 0.55 {
                0.95
            } else {
                0.95 * (1.0 - 0.45 * (u - 0.55) / 0.45)
            };
            let env = base * (0.88 + 0.12 * (u * 34.0 + b.seed * 40.0).sin());
            let cov_f = 255.0 * env * cfg.intensity * if b.branch { 0.72 } else { 1.0 };
            if cov_f < 1.0 {
                continue;
            }
            let cov = cov_f.min(255.0) as u8;
            // BURN-OUT: past the restrike the channel doesn't dim in place — it
            // RETRACTS from its origin toward the tip with a 20%-of-length
            // glowing edge (mirroring the beam's tail→head burn), so the strike
            // visibly withdraws into where it landed and disappears.
            let k_burn = if u > 0.55 { (u - 0.55) / 0.45 } else { 0.0 };
            let n1 = (b.pts.len() - 1).max(1) as f32;
            let burn = |i: usize, c: u8| -> u8 {
                let p = i as f32 / n1;
                ((c as f32) * ((p - k_burn) / 0.20).clamp(0.0, 1.0)) as u8
            };
            // Corona: the electric field around the channel, pure style hue.
            let corona_th = ch * if b.branch { 0.30 } else { 0.55 };
            verts.clear();
            for (i, &(x, y)) in b.pts.iter().enumerate() {
                verts.push(BeamVertex {
                    x,
                    y,
                    color: cfg.color,
                    cov: burn(i, (cov / 3).max(1)),
                });
            }
            comet_beam(out, geom.beam_clip(), verts, corona_th, 2, 0.0);
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            // Core: the channel itself — white-hot on main strikes (0.62 white
            // ceiling keeps every strike inside the beam's hue family, per the
            // monochrome law), pure hue on branches.
            let core_color = if b.branch {
                cfg.color
            } else {
                lerp_rgb(cfg.color, 0x00FF_FFFF, 0.62 * env)
            };
            let core_th = (ch * if b.branch { 0.10 } else { 0.18 }).max(1.5);
            verts.clear();
            for (i, &(x, y)) in b.pts.iter().enumerate() {
                verts.push(BeamVertex {
                    x,
                    y,
                    color: core_color,
                    cov: burn(i, cov),
                });
            }
            comet_beam(out, geom.beam_clip(), verts, core_th, 1, 0.0);
        }
    }

    /// Emit the VAPOR (steam + smoke + erase poofs) as growing, thinning radial
    /// halos. STEAM: pale blue-white, born small and bright, swelling ~3× while
    /// its coverage collapses — water flashing off hot steel. SMOKE: warm grey
    /// (ember light scattered through it near birth), swelling gently on a
    /// sinusoidal waft, lingering. POOF: neutral grey, brief — the erase puff
    /// any style may shed. Halos only — vapor has no square anywhere.
    pub(super) fn emit_vapor(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        halos: &mut Vec<RainHalo>,
    ) {
        if self.vapor.is_empty() {
            return;
        }
        // Steam + smoke are the FIRE style's thermal language and render only
        // under it (only fire paths ever spawn them, so this per-PARTICLE gate
        // is byte-identical to the old per-style early-out — it exists so the
        // style-agnostic erase POOF can render everywhere without a config
        // flip mid-life resurrecting foreign vapor).
        let fire = matches!(cfg.style, GlowStyle::Fire);
        let cell = geom.ch as f32;
        for v in self.vapor.iter().rev() {
            if !fire && v.kind != VaporKind::Poof {
                continue;
            }
            let age = now.saturating_duration_since(v.born).as_secs_f32();
            let t = (age / v.life).clamp(0.0, 1.0);
            let x = v.x0
                + v.vx * age
                + (age * (1.3 + 1.9 * v.seed) + v.seed * std::f32::consts::TAU).sin()
                    * cell
                    * 0.22
                    * t;
            let y = v.y0 + v.vy * age + 0.5 * v.gy * age * age;
            // LIGHT THEMES (P7): additive vapor cannot brighten white — the
            // puffs switch to SOURCE-OVER veils (straight RGB; the falloff
            // weight becomes per-pixel alpha): steam a cool pale mist, smoke
            // a warm grey shade, both reading naturally on any ground.
            let (r0, grow, color, peak) = match v.kind {
                VaporKind::Steam => (
                    cell * 0.26,
                    1.0 + 3.4 * t,
                    if cfg.dark_theme {
                        0x00C9_D6DE
                    } else {
                        0x00AB_BCC6
                    },
                    (150.0 * (1.0 - t) * (1.0 - t)) * cfg.intensity,
                ),
                VaporKind::Smoke => (
                    cell * 0.15,
                    1.0 + 2.4 * t,
                    if cfg.dark_theme {
                        // Ember light glows through young smoke, greying as it climbs.
                        lerp_rgb(0x0074_665A, 0x0044_403C, t)
                    } else {
                        lerp_rgb(0x0060_544A, 0x0084_7E78, t)
                    },
                    (52.0 * (1.0 - t)) * cfg.intensity,
                ),
                // The erase POOF — THE CLOUD: NEUTRAL grey (no ember warmth —
                // this is text going up in smoke, not fire thermals), darkening
                // as it thins on dark themes; on light themes a mid-grey
                // source-over veil with steam's stronger peak so it clears
                // `push_halo_over`'s perceptual floor while young.
                //
                // THE TWO GROUNDS NEED DIFFERENT PUFFS, and until 2026-08-28 the
                // dark branch only re-coloured the LIGHT one's geometry. A veil
                // on white starts as a small dark dot and earns its read from
                // CONTRAST; an additive puff on black earns its read from AREA
                // and has none to spare — `cell * 0.16` is a ~3 px seed at a
                // 20 px cell, so the dark cloud was a scatter of grey specks
                // measuring +36 peak luminance over a Nord ground for a whole
                // killed line. It is now born WIDER (0.34 cell) and swells less
                // (a cloud that trebles in size reads as an explosion), and its
                // opacity falls QUADRATICALLY from a punchier peak — a puff that
                // arrives and dissipates, rather than a grey smudge that lingers
                // at half strength for a second.
                //
                // THE PEAK IS A STACKING BUDGET, not a taste number. The puffs
                // CLUSTER, so the brightest pixel of a cloud is not one puff's
                // peak — it is the three or four that overlap near the collapse
                // point, ADDED. Over a Nord ground (blue channel 64) the blue
                // premultiplied by `0x9A_A0A6` clips at Σα ≈ 285, so a per-puff
                // 104 put a whole-line Ctrl-U's centre THROUGH white: measured
                // 106 newly-saturated pixels where a one-character Backspace
                // produced one. A clipped white blob is a flash, not smoke — and
                // it would also have taken the "caret is the brightest thing"
                // law with it. 84 keeps the deepest realistic stack (three puffs
                // ≈ 252) under the clip with the single-puff read intact, so the
                // cloud's SIZE says how much vanished and its brightness stays
                // put — and the caret is still the brightest thing on the row.
                VaporKind::Poof if cfg.dark_theme => (
                    cell * 0.34,
                    1.0 + 2.2 * t,
                    lerp_rgb(0x009A_A0A6, 0x005F_6368, t),
                    (84.0 * (1.0 - t) * (1.0 - t)) * cfg.intensity,
                ),
                VaporKind::Poof => (
                    cell * 0.16,
                    1.0 + 2.6 * t,
                    lerp_rgb(0x005A_6065, 0x008B_9196, t),
                    (140.0 * (1.0 - t) * (1.0 - t)) * cfg.intensity,
                ),
            };
            let peak = peak as u8;
            if peak == 0 {
                continue;
            }
            let r = r0 * grow;
            if cfg.dark_theme {
                push_halo(halos, geom, x, y, r * 1.35, r, premul_rgb(color, peak));
            } else {
                push_halo_over(halos, geom, x, y, r * 1.35, r, color, peak);
            }
        }
    }

    /// Emit the comet BODY as a smooth, crisp line of light instead of one opaque
    /// cell per swept Bresenham step (which staircases on diagonals). The swept
    /// cells become a polyline; [`comet_beam`] rasterizes it as an anti-aliased
    /// beam — a soft wide HALO under a bright HOT CORE — so a diagonal move reads
    /// as a clean glowing streak. The head run is extended to the live cursor
    /// cell so the beam stays visibly attached to the cursor.
    pub(super) fn emit_comet(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        // Styles whose streak is drawn elsewhere (Water = `emit_water`; rainbow kitty =
        // its own continuous ribbon) emit no shared additive beam. See `style_has_beam`.
        if !cfg.beam || self.sparks.is_empty() {
            return;
        }
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let oxf = geom.origin_x as f32;
        let center = |row: u16, col: u16| geom.cell_center(row, col);

        // Build the comet as RUNS of ADJACENT swept cells, so a hide/show gap or a
        // stale older group never connects across empty space. Each sample carries
        // its time-faded coverage and path position (tail 0 → head 1, for the hue
        // ramp). A fully-faded tail cell breaks the colour, not the run.
        // Reuse the resident run-builder scratch (spine AND per-run sample buffers
        // kept) instead of allocating a fresh nested Vec every animated frame:
        // clearing each entry in place keeps its heap buffer, where the old
        // `comet_runs.clear()` dropped every inner `Vec` and forced the next frame
        // to grow each run back from capacity 0. `runs_len` is the live watermark —
        // entries past it are emptied spares, so every consumer below reads
        // `..runs_len`, never the whole spine.
        for r in &mut self.comet_runs {
            r.clear();
        }
        let mut runs_len = 0usize;
        self.comet_run.clear();
        let mut prev: Option<(u16, u16)> = None;
        let mut head_cov = 0u8;
        for s in &self.sparks {
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            // TYPING sparks hold full brightness for 55% of life then cosine-fade,
            // so the CHAINED streak stays luminous across the inter-key gap instead
            // of sawtoothing toward zero before each key (the per-key pulse). LASER
            // typing sparks instead run the LIGHTNING-TRAIL discharge (full power →
            // bleed → lingering dim residual, see below) — still never dark
            // mid-rhythm, just dimmer down the tail. Jump
            // sparks keep the classic linear fade byte-identical — except LASER,
            // whose fired beam HOLDS at full power for 45% of life and then BURNS
            // OUT: a burn front sweeps tail→head with a glowing 15%-of-length
            // edge, so the ray visibly drains into the impact point and vanishes,
            // never dimming in place or getting chopped off mid-air. (All sparks
            // of one jump share born+life, so the front is coherent along the
            // whole beam.)
            let frac = (age / s.life).clamp(0.0, 1.0);
            let tf = if s.typing {
                if matches!(cfg.style, GlowStyle::Beam) {
                    // BEAM: the steady tube — full power, then one smooth cosine
                    // POWER-DOWN. No residual, no ripple, no burn front: the
                    // whole rod dims UNIFORMLY while `core_thick` below collapses
                    // it toward a hairline, so the light reads as an emitter
                    // switching off, not a comet dying or a charge draining.
                    beam_power(frac)
                } else if matches!(cfg.style, GlowStyle::Laser) {
                    // LIGHTNING TRAIL discharge: full beam power while freshly
                    // typed, then a cosine BLEED down to the residual charge,
                    // which lingers — flickering under the scintillation
                    // ripple below — before its own cosine collapse. The trail
                    // dims cell by cell behind the head, bright only at the
                    // freshly typed cell, so the long charged wake never parks
                    // full-power light over the text beneath it.
                    if frac < 0.20 {
                        1.0
                    } else if frac < 0.55 {
                        let k = (frac - 0.20) / 0.35;
                        Self::LASER_RESIDUAL
                            + (1.0 - Self::LASER_RESIDUAL)
                                * 0.5
                                * (1.0 + (std::f32::consts::PI * k).cos())
                    } else {
                        Self::LASER_RESIDUAL
                            * 0.5
                            * (1.0 + (std::f32::consts::PI * (frac - 0.55) / 0.45).cos())
                    }
                } else if frac < 0.55 {
                    1.0
                } else {
                    0.5 * (1.0 + (std::f32::consts::PI * (frac - 0.55) / 0.45).cos())
                }
            } else if matches!(cfg.style, GlowStyle::Laser) {
                if frac < 0.45 {
                    1.0
                } else {
                    let k = (frac - 0.45) / 0.55; // burn front, tail 0 → head 1
                    ((s.pos - k) / 0.15).clamp(0.0, 1.0) * (1.0 - 0.35 * k)
                }
            } else if matches!(cfg.style, GlowStyle::Beam) {
                // A jumped BEAM is one solid rod spanning the leap; it powers
                // down in place with the same uniform envelope as the typing
                // tube (all sparks of one jump share born+life, so the whole
                // rod dims and thins as one object).
                beam_power(frac)
            } else {
                1.0 - frac
            };
            let mut cov_f = (s.born_cov as f32) * tf * cfg.intensity;
            // FIRE legibility cap: the fire comet RIDES the typed text line, and
            // its head stacked with halos + bloom saturates the row to white —
            // erasing glyph contrast entirely (saturating add clamps both glyph
            // and light to 255). Cap the STREAK's coverage over the OCCUPIED
            // glyph cells (every swept spark is a just-typed glyph) to the
            // phaser's text-safety ceiling so the freshest letters keep contrast
            // THROUGH the flame — the amber-cap legibility law, now honoured at
            // the head too, not just the flame BODY above the line. The bridge
            // onto the EMPTY cursor cell keeps full punch below (FIRE_HEAD_COV_CAP).
            if matches!(cfg.style, GlowStyle::Fire) {
                cov_f = cov_f.min(Self::FIRE_STREAK_COV_CAP);
            }
            // PHASER text-safety ceiling (every other streaming style has one:
            // Rainbow kitty 150, meteor 135, zoom 118, water 76, fire 168): the
            // PHASER_LAYERS stack overlaps to 1.45× inside the full-height
            // core, so cap the per-sample coverage at 112 — the stacked
            // per-pixel sum stays ≈162, under saturation, and a same-hue glyph
            // can never clip pixel-identical into the band. At the shipped
            // default intensity (0.7) every emitted value is already below
            // this cap — the ceiling only bites overdriven configs.
            if matches!(cfg.style, GlowStyle::Phaser) {
                cov_f = cov_f.min(112.0);
            }
            // LASER scintillation: a ±14% energy ripple travelling head-ward
            // along the beam — the hum of a live emitter, not a static painted
            // stripe. Deterministic in (pos, age), so tests and the CPU/GPU
            // parity are unaffected; the ripple modulates around full power, so
            // it reads as surging, never as flicker toward dark.
            if matches!(cfg.style, GlowStyle::Laser) {
                cov_f *= 0.86 + 0.14 * (s.pos * 9.0 - age * 26.0).sin();
            }
            let cov = cov_f as u8;
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
                    if runs_len == self.comet_runs.len() {
                        self.comet_runs.push(Vec::new());
                    }
                    // SWAP, not `take`: the pooled (already-cleared) buffer at the
                    // watermark comes back into `comet_run`, so the next run starts
                    // with capacity instead of reallocating from zero.
                    let (runs, cur_run) = (&mut self.comet_runs, &mut self.comet_run);
                    std::mem::swap(&mut runs[runs_len], cur_run);
                    runs_len += 1;
                }
            }
            let (x, y) = center(s.row, s.col);
            // PHASER samples carry the cell's LAID hue as an offset from the
            // live sweep phase (`comet_color` adds `self.hue` back), so each
            // cell of the band keeps the colour it was laid in — a real
            // spatial rainbow — instead of the whole bar re-sampling one live
            // hue. Every other style keeps the tail→head path position.
            let pos = if matches!(cfg.style, GlowStyle::Phaser) {
                (s.hue - self.hue).rem_euclid(1.0)
            } else {
                s.pos
            };
            self.comet_run.push(CometSample { x, y, cov, pos });
            prev = Some(here);
            head_cov = cov;
        }
        if !self.comet_run.is_empty() {
            if runs_len == self.comet_runs.len() {
                self.comet_runs.push(Vec::new());
            }
            let (runs, cur_run) = (&mut self.comet_runs, &mut self.comet_run);
            std::mem::swap(&mut runs[runs_len], cur_run);
            runs_len += 1;
        }
        if runs_len == 0 {
            return;
        }
        // Connect the head run to the LIVE cursor cell so the beam visibly attaches
        // to the cursor (no trailing 1-cell gap → reads as responsive) — but ONLY
        // while the cursor is actually ADJACENT to the run's head. After a line
        // change or a leap the cursor is elsewhere; bridging then drew a stray
        // diagonal beam from the old text to the new position for as long as the
        // tail lived (the phaser "glitchy" bar hanging off the last typed word).
        // The bridge attaches at `cfg.head_dx` of the cell (0.5 = centre): a thin
        // BAR cursor sits at the cell's left edge, and a streak that overshoots
        // it by half a cell reads as detached from the cursor.
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            // The LIVE head run is at `runs_len - 1` (non-zero: the guard above
            // returned otherwise) — NOT `last_mut()`, which under the pool would
            // hand back a stale spare from an earlier, longer frame.
            && let Some(last) = self.comet_runs.get_mut(runs_len - 1)
            && let Some(head) = last.last().copied()
        {
            let (_, y) = center(cr, cc);
            // Window-absolute, mirroring `center` with `head_dx` in place of
            // 0.5. The `oxf +` term is load-bearing: without it the live head
            // vertex lands `pad` px left of the cursor in every real window and
            // the adjacency test below breaks — neither visible to the
            // origin-(0,0) identity suite.
            let x = oxf + (cc as f32 + cfg.head_dx) * cwf;
            if (x - head.x).abs() <= cwf * 1.5 && (y - head.y).abs() <= chf * 1.5 {
                // FIRE keeps FULL PUNCH over the EMPTY cursor cell: the swept
                // sparks are capped for legibility over the occupied glyph cells
                // (FIRE_STREAK_COV_CAP), but the bridge noses onto the blank
                // cursor cell where there is no glyph to bury — restore the head
                // to FIRE_HEAD_COV_CAP so the light still visibly leaves the
                // cursor. `comet_beam` ramps 108→168 across the bridge, so the
                // last glyph at the segment's tail is never re-washed.
                let bridge_cov = if matches!(cfg.style, GlowStyle::Fire) {
                    (head_cov as f32 * Self::FIRE_HEAD_COV_CAP / Self::FIRE_STREAK_COV_CAP)
                        .min(Self::FIRE_HEAD_COV_CAP) as u8
                } else {
                    head_cov
                };
                last.push(CometSample {
                    x,
                    y,
                    cov: bridge_cov,
                    // Phaser: the bridge is the segment being laid RIGHT NOW,
                    // so it leads at the live sweep phase (offset 0.5 = a fresh
                    // typing spark's laid hue) — the beam hue visibly leaves
                    // the cursor. Other styles: the head of the path ramp.
                    pos: if matches!(cfg.style, GlowStyle::Phaser) {
                        0.5
                    } else {
                        1.0
                    },
                });
            }
        }

        // LIGHT THEMES: additive light cannot brighten a white ground — worse,
        // the band's quads saturating-added OVER the freshly typed glyphs
        // lifted their ink toward the background's luminance (the worst
        // legibility failure: near-invisible fresh-typed text) while barely
        // reading as a streak at all. On a light theme the beam family
        // renders as SOURCE-OVER VEILS instead (the vapor's HaloMode::Over
        // law): per swept sample, two darkened saturated rails hugging the
        // row's edges — the streak reads on white as a deep-toned tube
        // bracketing the text, the row's glyph band itself stays veil-free
        // (rails' falloff dies right at the row edge), and dark ink under
        // the skirts only ever darkens — never lifts toward the ground.
        if !cfg.dark_theme {
            for run in &self.comet_runs[..runs_len] {
                for s in run {
                    // Radius-scale peak: strong while the spark is fresh,
                    // culled by push_halo_over's perceptual floor (<96) about
                    // two-thirds into the fade — the vapor's fade law.
                    let peak = (s.cov as f32 * 1.9).min(230.0) as u8;
                    let color = lerp_rgb(self.comet_color(cfg, s.pos), 0x0000_0000, 0.38);
                    // rx spans past the cell so neighbouring veils MERGE into a
                    // continuous rail (push_halo_over scales radii by peak, so
                    // the drawn reach lands near one cell width).
                    push_halo_over(
                        halos,
                        geom,
                        s.x,
                        s.y - chf * 0.62,
                        cwf * 1.55,
                        chf * 0.52,
                        color,
                        peak,
                    );
                    push_halo_over(
                        halos,
                        geom,
                        s.x,
                        s.y + chf * 0.62,
                        cwf * 1.55,
                        chf * 0.52,
                        color,
                        peak,
                    );
                }
            }
            return;
        }

        // BEAM power-down: the tube's thickness rides the NEWEST spark's power
        // envelope. While the emitter is live (typing rhythm / fresh jump) the
        // rod holds full girth; once the light starts dying the whole tube
        // THINS toward a hairline in step with the uniform dimming above —
        // the two together are the switch-off. (The newest spark is the head;
        // sparks of one jump share born+life, so the collapse is coherent.)
        let beam_pw = if matches!(cfg.style, GlowStyle::Beam) {
            self.sparks
                .last()
                .map(|s| {
                    let age = now.saturating_duration_since(s.born).as_secs_f32();
                    beam_power((age / s.life).clamp(0.0, 1.0))
                })
                .unwrap_or(1.0)
        } else {
            1.0
        };
        // Phaser runs a FAT beam core — the FULL cell height (vs the shared
        // ~1/8): with its tight PHASER_LAYERS halo it reads as a solid
        // textbox-height bar of spectrum behind the cursor, not a hairline.
        let core_thick = match cfg.style {
            // Phaser's band spans MOST of the row — still the fat bar of
            // spectrum — but leaves the cap-height and descender bands clear
            // so the letters riding it stay legible (a full-cell core buries
            // ascenders and descenders).
            GlowStyle::Phaser => (chf * 0.66).max(8.0),
            // Fire burns FAT: the core swells with the eased temperature — a
            // smolder is a slim vein of heat, a full blaze a rope of molten
            // light, kept thin enough that the rope never buries the glyphs it
            // crosses.
            GlowStyle::Fire => (chf * (0.13 + 0.09 * self.fire_t())).max(2.0),
            // FAT charged conductor (~2× the shared core): the lightning strikes
            // fork off a thick high-voltage cable of light, not a hairline.
            GlowStyle::Laser => (chf * 0.26).max(3.5),
            // The BEAM tube: a solid rod (~1/3 cell) at full power, collapsing
            // toward a hairline as the light powers down (`beam_pw` above).
            GlowStyle::Beam => (chf * (0.08 + 0.26 * beam_pw)).max(2.0),
            // SPARKLE pours a WIDE rainbow ribbon (~0.45 cell, between laser's
            // cable and phaser's bar): the hairline default reads as blank, and
            // the celestial shapes the wake sheds (stars / moons / mini-comets)
            // need a band of sky to fall out of, not a wire. Cap-height and
            // descender bands stay clear, phaser's law.
            GlowStyle::Sparkle => (chf * 0.45).max(5.0),
            _ => (chf * 0.13).max(2.0), // crisp thin core
        };
        // Flatten the per-cell Bresenham path to the true straight move so the beam
        // is a clean diagonal, not a stair-stepped polyline of cell centres.
        let straighten = cwf.max(chf) * 0.8;

        // The Laser arm's per-layer filament polyline. Taken via the same
        // `mem::take` idiom `tick` uses for the halo/patch/bolt scratches (a
        // `&mut self.comet_verts` binding cannot live across the loop: the body
        // calls `self.comet_color`, a whole-`self` shared borrow). Taken HERE —
        // past every early return above — so no exit path can drop the capacity,
        // and restored unconditionally right after the loop.
        let mut comet_verts = std::mem::take(&mut self.comet_verts);
        for r in &self.comet_runs[..runs_len] {
            if r.len() < 2 {
                // A lone swept cell (e.g. a single move with the cursor hidden): a
                // small soft dot, not a full blocky cell.
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
            // The layered-bloom looks live ONCE in `aterm_render`, shared with
            // the render demos so the live aurora and the previews can't drift.
            // Laser gets its own stack: a razor filament inside a wide smooth
            // same-hue halo, with fine strides so the falloff is continuous
            // (the comet's coarse 3px aura slabs are what read as pixelated).
            // The phaser too: its near-cell-height core needs the tight
            // PHASER_LAYERS halo (the comet's 5.5× aura would wash whole rows).
            match cfg.style {
                GlowStyle::Laser => {
                    // The razor FILAMENT + tight beam body: the INNER laser layers
                    // only (thickness×core ≤ 2.9, i.e. ≤ ~0.75 cell), drawn with
                    // fine 1px strides so the needle stays crisp. Tuple shape:
                    // (thickness×, coverage×, white-mix base, white-mix ×pos).
                    const LASER_CORE: [(f32, f32, f32, f32); 3] = [
                        (2.9, 0.60, 0.0, 0.0),   // inner glow (pure hue)
                        (1.5, 0.95, 0.10, 0.0),  // beam body
                        (0.6, 1.00, 0.26, 0.42), // white-hot filament, hotter to the head
                    ];
                    // The polyline buffer is the resident `comet_verts` scratch
                    // taken above (it was a fresh `Vec::with_capacity(r.len())`
                    // per run per frame). `clear()` below already resets it at the
                    // top of every layer pass and `comet_beam` only ever reads the
                    // filled prefix, so the emitted quads are byte-identical.
                    for &(tmul, cmul, mbase, mpos) in &LASER_CORE {
                        comet_verts.clear();
                        for s in r {
                            let style = self.comet_color(cfg, s.pos);
                            let color = if mbase > 0.0 || mpos > 0.0 {
                                lerp_rgb(style, 0x00FF_FFFF, (mbase + mpos * s.pos).min(1.0))
                            } else {
                                style
                            };
                            comet_verts.push(BeamVertex {
                                x: s.x,
                                y: s.y,
                                color,
                                cov: (s.cov as f32 * cmul) as u8,
                            });
                        }
                        comet_beam(
                            out,
                            geom.beam_clip(),
                            &comet_verts,
                            core_thick * tmul,
                            1,
                            straighten,
                        );
                    }
                    // The WIDE neon AURA must be RADIAL halos, never per-cell-row
                    // quad slabs: stacked that way the 5.4×/9.5×/16× halo layers
                    // POSTERIZE into horizontal bands and drop a detached
                    // cell-quantized quad in the row below the cursor. Radial
                    // keeps the falloff continuous and never quantized to a row
                    // boundary. One halo per swept sample; overlapping
                    // neighbours merge into one glowing tube hugging the filament,
                    // in the pure beam hue (the monochrome law).
                    for s in r {
                        let peak = (s.cov as f32 * 0.42).min(150.0) as u8;
                        if peak == 0 {
                            continue;
                        }
                        push_halo(
                            halos,
                            geom,
                            s.x,
                            s.y,
                            cwf * 1.35,
                            chf * 1.05,
                            premul_rgb(cfg.color, peak),
                        );
                    }
                }
                GlowStyle::Phaser => {
                    // NO RDP straightening for the phaser: its runs are always
                    // same-row (row changes clear/split the path), so the
                    // simplification only ever DROPS the interior samples —
                    // and with them every cell's LAID hue: the rasterizer then
                    // lerps tail-endpoint→head-endpoint, collapsing the
                    // spatial spectrum to one near-uniform colour that
                    // retroactively re-lerps across the WHOLE band on every
                    // hue step (a lockstep strobe). Keeping every
                    // sample renders each cell in the hue it was laid in —
                    // the real spatial rainbow. Geometry is identical (the
                    // samples are collinear); cost is O(live samples).
                    phaser_streak_quads(out, geom.beam_clip(), r, core_thick, 0.0, &|pos| {
                        self.comet_color(cfg, pos)
                    });
                }
                GlowStyle::Beam => {
                    beam_glow_quads(out, geom.beam_clip(), r, core_thick, straighten, &|pos| {
                        self.comet_color(cfg, pos)
                    });
                }
                _ => {
                    comet_glow_quads(out, geom.beam_clip(), r, core_thick, straighten, &|pos| {
                        self.comet_color(cfg, pos)
                    });
                }
            }
        }
        // Restore the filament scratch UNCONDITIONALLY (no exit path lies between
        // the take and here), so its capacity survives to the next frame.
        self.comet_verts = comet_verts;
    }

    /// Resolve a **Trail Pack** ramp at path position `pos` (0 tail → 1 head).
    /// This is the interpreter's OWN colour closure — the custom path never
    /// calls the shared [`style_comet_color`] (whose `Custom` arm is a mere
    /// exhaustiveness fallback), so a pack owns its palette entirely. Uses the
    /// engine's rolling `self.hue` as the temporal phase, exactly like the
    /// built-in ramps.
    pub(super) fn custom_ramp_color(&self, p: &TrailParams, cfg: &GlowConfig, pos: f32) -> u32 {
        match p.ramp {
            RampParams::Mono => cfg.color,
            RampParams::HsvSweep {
                span_deg, sat, val, ..
            } => {
                let h = (self.hue + pos * (span_deg / 360.0)).rem_euclid(1.0);
                hsv2rgb(h, sat, val)
            }
            RampParams::Stops { stops, n, .. } => {
                let n = n as usize;
                if n == 0 {
                    return cfg.color;
                }
                if n == 1 {
                    return stops[0].1;
                }
                // Bracket `pos` between the two nearest stops (stops are authored
                // in non-decreasing `t`; the compiler bounds each to 0..=1).
                let mut lo = 0usize;
                for (i, stop) in stops.iter().enumerate().take(n) {
                    if stop.0 <= pos {
                        lo = i;
                    }
                }
                let hi = (lo + 1).min(n - 1);
                let (t0, c0) = stops[lo];
                let (t1, c1) = stops[hi];
                let f = if (t1 - t0).abs() < 1e-6 {
                    0.0
                } else {
                    ((pos - t0) / (t1 - t0)).clamp(0.0, 1.0)
                };
                lerp_rgb(c0, c1, f)
            }
            RampParams::Bands { bands, n } => {
                let n = (n as usize).max(1);
                bands[((pos * n as f32) as usize).min(n - 1)]
            }
        }
    }

    /// The **Trail Pack** interpreter — the SINGLE new emit branch reached only
    /// when `cfg.pack` is `Some` (see the wrap in [`Self::tick`]). It DRIVES the
    /// existing rasterizers/helpers from the pack's [`TrailParams`]; it never
    /// calls the per-style `emit_comet`/`emit_crown`/`emit_particles` bodies
    /// (those stay hardcoded and byte-identical), and no built-in path reads
    /// `cfg.pack`. Every custom sample flows through this function's own
    /// emission funnels, where the structural legibility ceiling
    /// ([`Self::CUSTOM_COV_CAP`]) is applied — a pack cannot bypass it.
    ///
    /// Takes `&mut self` solely so the beam funnel can reuse the resident
    /// `comet_runs`/`comet_run` scratch the built-in twin already uses (the two
    /// arms are mutually exclusive per frame and both clear on entry); every
    /// helper it calls is still `&self`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_custom(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
        p: &TrailParams,
    ) {
        #[cfg(test)]
        CUSTOM_EMIT_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let (oxf, oyf) = (geom.origin_x as f32, geom.origin_y as f32);
        let center = |row: u16, col: u16| geom.cell_center(row, col);

        // ---- BEAM channel (additive GlowQuads) ----
        if p.channels.glow_add && p.beam.enabled && !self.sparks.is_empty() {
            // Build contiguous runs of swept cells exactly as `emit_comet` does,
            // but the fade envelope is the PACK's and every sample coverage is
            // hard-clamped to the structural ceiling. This CometSample builder is
            // the sole beam funnel — a pack cannot emit a sample past the cap.
            // Reuse the SAME resident run-builder scratch the built-in twin uses
            // (this arm used to build a whole nested `Vec` from scratch every
            // frame). Clearing each pooled entry in place is MANDATORY, not
            // optional: without it the previous frame's samples would render as a
            // ghost beam. `runs_len` is the live watermark — see the field doc.
            for r in &mut self.comet_runs {
                r.clear();
            }
            let mut runs_len = 0usize;
            self.comet_run.clear();
            let mut prev: Option<(u16, u16)> = None;
            let mut head_cov = 0u8;
            for s in &self.sparks {
                let age = now.saturating_duration_since(s.born).as_secs_f32();
                let frac = (age / s.life).clamp(0.0, 1.0);
                let mut tf = match p.beam.envelope {
                    Envelope::Linear => 1.0 - frac,
                    Envelope::HoldCosine { hold_frac } => {
                        if frac < hold_frac {
                            1.0
                        } else {
                            let k =
                                ((frac - hold_frac) / (1.0 - hold_frac).max(1e-3)).clamp(0.0, 1.0);
                            0.5 * (1.0 + (std::f32::consts::PI * k).cos())
                        }
                    }
                    Envelope::BurnOut {
                        hold_frac,
                        front_width,
                    } => {
                        if frac < hold_frac {
                            1.0
                        } else {
                            let k =
                                ((frac - hold_frac) / (1.0 - hold_frac).max(1e-3)).clamp(0.0, 1.0);
                            ((s.pos - k) / front_width.max(1e-3)).clamp(0.0, 1.0) * (1.0 - 0.35 * k)
                        }
                    }
                };
                if let Some(sc) = p.beam.scint {
                    // A travelling ±amp ripple (deterministic in pos+age), so the
                    // beam hums like a live emitter without touching parity.
                    tf *= (1.0 - sc.amp) + sc.amp * (s.pos * sc.freq - age * 26.0).sin().abs();
                }
                let cov_f = ((s.born_cov as f32) * tf * cfg.intensity).min(Self::CUSTOM_COV_CAP);
                let cov = cov_f as u8;
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
                    if far && !self.comet_run.is_empty() {
                        if runs_len == self.comet_runs.len() {
                            self.comet_runs.push(Vec::new());
                        }
                        // SWAP, not `take`: the pooled (already-cleared) buffer at
                        // the watermark comes back into `comet_run`.
                        let (runs, cur_run) = (&mut self.comet_runs, &mut self.comet_run);
                        std::mem::swap(&mut runs[runs_len], cur_run);
                        runs_len += 1;
                    }
                }
                let (x, y) = center(s.row, s.col);
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
                if runs_len == self.comet_runs.len() {
                    self.comet_runs.push(Vec::new());
                }
                let (runs, cur_run) = (&mut self.comet_runs, &mut self.comet_run);
                std::mem::swap(&mut runs[runs_len], cur_run);
                runs_len += 1;
            }
            // Bridge the head run to the live cursor cell while adjacent, exactly
            // as `emit_comet` does (the streak visibly leaves the cursor).
            // The `runs_len > 0` test reproduces the old `runs.last_mut()`
            // exactly — the chain short-circuits when no run was published this
            // frame — and indexing the watermark rather than `last_mut()` keeps a
            // stale pooled spare from an earlier, longer frame out of the bridge.
            if let Some((cr, cc)) = cur
                && (cr as usize) < geom.rows
                && (cc as usize) < geom.cols
                && runs_len > 0
                && let Some(last) = self.comet_runs.get_mut(runs_len - 1)
                && let Some(head) = last.last().copied()
            {
                let (_, y) = center(cr, cc);
                let x = oxf + (cc as f32 + cfg.head_dx) * cwf;
                if (x - head.x).abs() <= cwf * 1.5 && (y - head.y).abs() <= chf * 1.5 {
                    last.push(CometSample {
                        x,
                        y,
                        cov: head_cov,
                        pos: 1.0,
                    });
                }
            }

            let core_px = (chf * (p.beam.cell_frac + p.beam.heat_gain * self.heat)).max(2.0);
            let straighten = cwf.max(chf) * 0.8;
            let layers = p.beam_layer_tuples();
            let n = (p.beam.layer_count as usize).min(layers.len());
            // LIGHT-THEME TREATMENT (the pack's `theme` arm). Additive light cannot
            // brighten a white ground, so on a LIGHT theme an OverVeil/DarkenTints
            // pack renders the beam as SOURCE-OVER veil rails (the built-in beam
            // family's automatic light adaptation) — two darkened saturated rails
            // hugging the row's edges that read on white. DarkenTints tints them
            // deeper so a saturated hue survives source-over. `Auto` keeps the
            // additive beam (the current custom behavior). Dark themes ALWAYS draw
            // the additive beam, so the arm only ever changes the light path —
            // every built-in is untouched (it never reaches `emit_custom`).
            let light_veil =
                !cfg.dark_theme && matches!(p.theme, ThemeArm::OverVeil | ThemeArm::DarkenTints);
            if light_veil {
                let darken = if matches!(p.theme, ThemeArm::DarkenTints) {
                    0.62
                } else {
                    0.38
                };
                for r in &self.comet_runs[..runs_len] {
                    for s in r {
                        let peak = ((s.cov as f32) * 1.9).min(230.0) as u8;
                        let color =
                            lerp_rgb(self.custom_ramp_color(p, cfg, s.pos), 0x0000_0000, darken);
                        push_halo_over(
                            halos,
                            geom,
                            s.x,
                            s.y - chf * 0.62,
                            cwf * 1.55,
                            chf * 0.52,
                            color,
                            peak,
                        );
                        push_halo_over(
                            halos,
                            geom,
                            s.x,
                            s.y + chf * 0.62,
                            cwf * 1.55,
                            chf * 0.52,
                            color,
                            peak,
                        );
                    }
                }
            } else {
                for r in &self.comet_runs[..runs_len] {
                    if r.len() < 2 {
                        if let Some(s0) = r.first() {
                            let s = core_px.max(2.0) as i32;
                            push_rect(
                                out,
                                geom,
                                s0.x as i32 - s / 2,
                                s0.y as i32 - s / 2,
                                s,
                                s,
                                premul_rgb(self.custom_ramp_color(p, cfg, s0.pos), s0.cov),
                            );
                        }
                        continue;
                    }
                    custom_beam_quads(
                        out,
                        geom.beam_clip(),
                        r,
                        core_px,
                        straighten,
                        &|pos| self.custom_ramp_color(p, cfg, pos),
                        &layers[..n],
                    );
                }
            }
            // BED channel (`channels.bed`): a faint ADDITIVE cell-body fill under
            // the beam — the pack analog of the built-in comet's "cell body
            // supplies body, the beam supplies continuity". Clamped WELL under the
            // structural ceiling so it stays a translucent body the glyph reads
            // through, and skipped on the light-veil path (a white ground swallows
            // additive light — the veil rails already carry the streak there).
            if p.channels.bed && !light_veil {
                let w = (cwf as i32).max(1);
                let h = (chf as i32).max(1);
                for r in &self.comet_runs[..runs_len] {
                    for s in r {
                        let cov = ((s.cov as f32) * 0.30).min(Self::CUSTOM_COV_CAP * 0.45) as u8;
                        if cov == 0 {
                            continue;
                        }
                        push_rect(
                            out,
                            geom,
                            s.x as i32 - w / 2,
                            s.y as i32 - h / 2,
                            w,
                            h,
                            premul_rgb(self.custom_ramp_color(p, cfg, s.pos), cov),
                        );
                    }
                }
            }
            out.truncate(Self::MAX_QUADS);
        }

        // ---- CROWN channel (RainHalo) ----
        // A compact radial bloom on the cursor cell, gated on the SHARED last-move
        // window (stamped by the shared spawn logic). Dark-theme only, mirroring
        // the built-in non-fire crown (additive light washes a white ground).
        if p.crown.enabled
            && cfg.dark_theme
            && matches!(p.channels.halo, HaloChannel::Add | HaloChannel::Over)
            // `last_move` is also the fast-glide clock and therefore is not an
            // emission licence.  The one explicit crown arm is authoritative:
            // clearing it on a denied/reset move must make this channel dark.
            && self.crown_until.is_some_and(|until| now < until)
            && let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && let Some(t0) = self.last_move
        {
            let age_ms = now.saturating_duration_since(t0).as_millis() as u64;
            let window = self.crown_window_ms.max(1);
            if age_ms < window {
                let fade = 1.0 - age_ms as f32 / window as f32;
                let ccx = oxf + cc as f32 * cwf + cwf * 0.5;
                let ccy = oyf + cr as f32 * chf + chf * 0.5;
                let peak =
                    (p.crown.peak_cov * 255.0 * 1.9 * fade * cfg.intensity * self.typing_boost())
                        .min(210.0);
                if peak >= 1.0 {
                    let color = self.custom_ramp_color(p, cfg, 1.0);
                    push_halo(
                        halos,
                        geom,
                        ccx,
                        ccy,
                        cwf * p.crown.radius_cells * 1.4,
                        chf * p.crown.radius_cells,
                        premul_rgb(color, peak as u8),
                    );
                }
            }
        }

        // ---- RING channel ----
        // Reuse the built-in ring geometry from the SHARED `self.ring` state (set
        // by the shared jump logic). Its colour match resolves `Custom` to accent.
        if p.ring.enabled {
            self.emit_ring(now, cfg, geom, out, halos);
        }

        // ---- PARTICLES ----
        // Draw the pack populations spawned into the SHARED collection: a generic
        // dot coloured from the pack ramp, dimmed over an occupied glyph cell (the
        // shared row probe) and hard-clamped to the structural ceiling.
        for pt in self.particles.iter().rev() {
            if out.len() >= Self::MAX_QUADS {
                break;
            }
            let age = now.saturating_duration_since(pt.born).as_secs_f32();
            let t = (age / pt.life).clamp(0.0, 1.0);
            let fade = 1.0 - t;
            let x = pt.x0 + pt.vx * age;
            let y = pt.y0 + pt.vy * age + 0.5 * pt.gy * age * age;
            let mut cov_f = 230.0 * fade * cfg.intensity;
            if self.own_row_glyph_at_px(geom, x, y) {
                cov_f *= 0.30;
            }
            cov_f = cov_f.min(Self::CUSTOM_COV_CAP);
            let cov = cov_f as u8;
            if cov == 0 {
                continue;
            }
            let color = self.custom_ramp_color(p, cfg, pt.hue);
            // `cov_scale` carries the population's dot-size scale (see the custom
            // spawn block); a dot never smaller than a device pixel.
            let sz = ((chf * pt.cov_scale).max(1.5)) as i32;
            let (ix, iy) = (x as i32, y as i32);
            push_rect(
                out,
                geom,
                ix - sz / 2,
                iy - sz / 2,
                sz.max(1),
                sz.max(1),
                premul_rgb(color, cov),
            );
        }
    }

    /// CAMPAIGN 2 — the PER-PIXEL FIRE: each hot cell of the envelope emits
    /// [`FirePatch`]es (split per row band, shared root/phase so the field is
    /// seamless) that both backends evaluate at every device pixel through the
    /// shared integer field — real licking tongues, continuous black-body
    /// blending, zero texel granularity. Dark themes composite additively;
    /// light themes get the ink-fire ([`FireMode::Over`]). The per-cell
    /// ENGULFMENT integral derives analytically from the same envelope
    /// parameters, fading with the live coverage exactly as the old texel
    /// integral did — and feeds the CONTRAST-HALO strengths (`halo_cells`),
    /// the colour-free legibility stream (the ink never recolours).
    #[allow(clippy::too_many_arguments)] // the cool-veil halo sink crossed the arity line
    pub(super) fn emit_flames(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        patches: &mut Vec<FirePatch>,
        // Kept in the signature for the day charring returns in a form that
        // does NOT read as lag; empty every frame (see NO CHAR, NO VEIL below).
        _charred: &mut Vec<CharFg>,
        halo_cells: &mut Vec<FireHaloCell>,
        engulf: &mut Vec<(u16, u16, f32)>,
        _halos: &mut Vec<RainHalo>,
    ) {
        engulf.clear();
        if !matches!(cfg.style, GlowStyle::Fire) || self.sparks.is_empty() {
            return;
        }
        let ch = geom.ch as f32;
        let t = self.fire_t();
        // LEGIBILITY-FIRST coverage ceiling: the fire crosses live text, so the
        // additive/ink field must stay a translucent TINT the glyph shows
        // through — never a wash. Kept well under the wash point; the drama
        // comes from HEIGHT, MOTION, and the rising wall above the line, not
        // from opacity over the letters. Kept low enough that even the
        // white-hot HEAD stays translucent enough to read the letters under it —
        // legibility is non-negotiable; the flame reads just as hot at 44/255.
        let cov_cap = (22.0 + 12.0 * t).min(255.0) as u8;
        // PER-CELL HEAT ENVELOPE: the strongest spark per cell wins, newest
        // first. Keep 12 cells at full strength plus a 4-cell FADING tail. A
        // hard 12-cap makes the oldest still-burning cell vanish in one frame at
        // strength ~0.5 — the fire wall's tail chopping cell-by-cell during fast
        // typing. The extra slots let that tail DISSOLVE instead: their rank
        // fade (below) ramps them to zero, so the next eviction is imperceptible.
        let mut cells: [(u16, u16, f32); 16] = [(0, 0, 0.0); 16];
        let mut n_cells = 0usize;
        for s in self.sparks.iter().rev() {
            if (s.row as usize) >= geom.rows || (s.col as usize) >= geom.cols {
                continue;
            }
            // EFFICIENCY: do the cheap cell-membership test BEFORE the
            // transcendentals. A spark whose cell is neither already kept nor a
            // free slot (the 13th+ distinct cell) is dropped regardless of its
            // strength, so computing its env/powf/cos was pure waste. Byte-exact:
            // kept and newly-added cells run the identical env/powf/max as before.
            let slot = cells[..n_cells]
                .iter()
                .position(|c| c.0 == s.row && c.1 == s.col);
            if slot.is_none() && n_cells >= cells.len() {
                continue;
            }
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            let u = (age / s.life).clamp(0.0, 1.0);
            let env = if s.typing {
                const EMBER: f32 = 0.38;
                if u < 0.10 {
                    1.0
                } else if u < 0.40 {
                    let k = (u - 0.10) / 0.30;
                    EMBER + (1.0 - EMBER) * 0.5 * (1.0 + (std::f32::consts::PI * k).cos())
                } else {
                    EMBER * 0.5 * (1.0 + (std::f32::consts::PI * (u - 0.40) / 0.60).cos())
                }
            } else {
                1.0 - u
            };
            // TRAIL IDENTITY: steep head→tail falloff — flames at the head,
            // dying embers a few cells back, darkness at the tail.
            let strength = env * (0.16 + 0.84 * s.pos.powf(1.4));
            if strength < 0.02 {
                continue;
            }
            match slot {
                Some(i) => cells[i].2 = cells[i].2.max(strength),
                None => {
                    cells[n_cells] = (s.row, s.col, strength);
                    n_cells += 1;
                }
            }
        }
        if n_cells == 0 {
            return;
        }
        // One quantized phase per frame: both backends see the identical time.
        let phase = (self.flame_phase * 1024.0) as u32;
        let temp = (t * 255.0) as u8;
        // Tongues drag BEHIND rightward typing: lean/4 px at full rise.
        let lean_q = (-(ch * 0.30 * t) * 4.0).clamp(-127.0, 127.0) as i8;
        let mode = if cfg.dark_theme {
            FireMode::Add
        } else {
            FireMode::Over
        };
        // EFFECTS-BOX horizontal clamp (identity-exact at head 0: the box IS the
        // grid box, matching the historical grid-relative clamps).
        let (fx_l, fx_r) = (geom.fx_left(), geom.fx_right());
        for (rank, &(row, col, strength)) in cells[..n_cells].iter().enumerate() {
            // TAIL DISSOLVE (see the 16-slot envelope above): the first 12 cells
            // keep full strength; the trailing tail fades linearly toward zero so
            // the oldest still-burning cell drops below the `peak < 1.5` cull as a
            // whisper, never as a full-height column snapping off.
            let strength = if rank < 12 {
                strength
            } else {
                strength * ((cells.len() - rank) as f32 / (cells.len() - 11) as f32).min(1.0)
            };
            // EARNED drama: the height ramps QUADRATICALLY in the eased
            // temperature, so it stays calm in casual typing and towers only
            // under sustained typing / key-hold. A lone peck is a TINY lick — a
            // sixth of a cell — and sustained key-repeat cadence (inter-key ≤
            // HEAT_GAP_FULL) builds toward a ~6-cell wall. The quadratic keeps
            // the ramp reading as acceleration, not a switch; cov_cap keeps it
            // translucent over text, so even the towering wall stays legible.
            let peak = ch * (0.16 + 5.80 * t * t) * strength;
            if peak < 1.5 {
                continue;
            }
            // Root the flame at the CELL TOP so it rises into the inter-line
            // space + the row ABOVE — the line being TYPED stays clear, the
            // dense root never sits over the fresh glyph the user is reading.
            // TOP-ROW rise room: both clamps are GRID-RELATIVE and relaxed by
            // exactly `geom.head` (the chrome band), then translated by
            // origin — NEVER window-clamped directly, so at head == 0 every
            // operand is byte-identical to the pad-relative math regardless of
            // pad/strip (the identity law; a window-clamped form instead shifts
            // row-0 roots by `pad`).
            // With head == 0 the root clamps at least one cell down; with head ≥ ch the NATURAL cell-top
            // root is restored and row-0 flames climb into the chrome band;
            // the field's top FADE (fire_field::fire_top_fade) dissolves
            // whatever approaches the frame edge.
            let head = i32::from(geom.head);
            let oy = geom.origin_y as i32;
            let root_floor = (geom.ch as i32 - head).max(2);
            let base_y = oy + ((row as f32 * ch + 2.0) as i32).max(root_floor);
            // Rise ceiling: the grid top relaxed by the head band — at head 0
            // this is the grid top exactly (the historical clamp).
            let top_y = (base_y - peak as i32 - 2).max(oy - head);
            if top_y >= base_y {
                continue;
            }
            // SHIFT the sampling window in the LEAN direction instead of widening
            // it on BOTH sides. The field shears the sample INTERNALLY
            // (`shear = lean·vn/4`), so the window only needs to FOLLOW the
            // tongue's lean, not bracket it. Symmetric widening made every cell's
            // patch overlap both neighbours by 2·lean_px, and — since the field is
            // a pure function of ABSOLUTE x — the overlapping patches stacked
            // ADDITIVELY into a periodic bright picket-fence seam at every cell
            // boundary (up to 2× cov_cap). A uniform shift makes adjacent same-row
            // patches TILE (each pixel covered exactly once), killing the
            // double-add while the leaning tongues still render.
            let lean_px = (lean_q as i32).abs() / 4 + 1;
            let shift = if lean_q > 0 { lean_px } else { -lean_px };
            let ox = geom.origin_x as i32;
            let x0 = (ox + (col as i32) * geom.cw as i32 + shift).max(fx_l);
            let x1 = (ox + ((col as i32) + 1) * geom.cw as i32 + shift).min(fx_r);
            if x1 <= x0 {
                continue;
            }
            // Split per row band (the dirty gate stays exact); every band quad
            // shares base_y/phase/params, so the field is seamless. Bands
            // anchor at origin_y (the grid top): ABOVE-GRID bands come out
            // negative and are NOT rejected — they emit with damage-hint row 0
            // (row tags are grid-row hints; row 0 opens the top scissor band),
            // so fire climbing into the chrome band still presents.
            let chi = geom.ch as i32;
            // ROOT SKIRT extent: the field now dissolves (mirrored, 5x
            // compressed) BELOW the root through the glyph row's top — emit the
            // covering bands too, clipped to the effects box. Under-ink slot:
            // the letters still paint over it.
            // (`chi.min(3)` keeps the clamp well-formed on degenerate geometries —
            // a 1px-cell bench grid would otherwise panic on min > max.)
            let skirt_end = (base_y + (peak as i32 / 4).clamp(chi.min(3), chi)).min(geom.fx_bot());
            let mut yy = top_y;
            while yy < skirt_end {
                if patches.len() >= Self::MAX_QUADS {
                    return;
                }
                let band = (yy - oy).div_euclid(chi);
                let band_end = (oy + (band + 1) * chi).min(skirt_end);
                if band < geom.rows as i32 {
                    patches.push(FirePatch {
                        row: band.max(0) as u16,
                        x: x0 as u16,
                        y: yy as u16,
                        w: (x1 - x0) as u16,
                        h: (band_end - yy) as u16,
                        base_y: base_y as u16,
                        peak_h: (peak as u16).max(1),
                        phase,
                        temp,
                        strength: (strength * 255.0) as u8,
                        lean: lean_q,
                        cov_cap,
                        cell_h: geom.ch as u16,
                        mode,
                    });
                    // ENGULFMENT for the charred ink: how much of this band the
                    // flame body plausibly covers — the analytic twin of the old
                    // texel integral (band-height share × envelope × density).
                    // Gated on `band >= 0` SEPARATELY from the patch push: an
                    // above-grid band has no glyph to char, and a negative band
                    // must never reach `band as u16` (the wrap would key a
                    // garbage charred row).
                    if band >= 0 {
                        let band_frac = (band_end - yy) as f32 / ch;
                        let rise_frac =
                            1.0 - ((base_y - band_end) as f32 / peak).clamp(0.0, 1.0) * 0.55;
                        let wgt = strength * band_frac * rise_frac * (0.35 + 0.55 * t);
                        let key = (band as u16, col);
                        match engulf.iter_mut().find(|e| (e.0, e.1) == key) {
                            Some(e) => e.2 += wgt,
                            None => engulf.push((key.0, key.1, wgt)),
                        }
                    }
                }
                yy = band_end;
            }
        }
        // NO CHAR, NO VEIL: a charred letter that dims and then pops back reads
        // as LAG even at 56 fps with a 34 ms worst present gap (measured). Ink
        // NEVER changes color (the no-recolor
        // law) — nothing to mistake for latency. What the engulf accumulator
        // feeds instead is the CONTRAST HALO: a colour-free STRENGTH per
        // engulfed cell, driving the dark dilation ring the renderers stamp
        // AROUND the glyph's strokes (over the flame, under the untouched
        // ink) so the letterform separates from a bright blaze — the owner's
        // "can't read white text over the very bright flame". Emitted for
        // EVERY engulfed cell on ANY row: the halo protects text everywhere
        // the flame covers — it does not recolor, so no cursor-row/freshness
        // gate applies. Below 0.15
        // the flame is a wisp, not an engulfment — no ring (and no pop-in:
        // the renderer's alpha floor keeps the first ring a whisper).
        // DARK THEME ONLY: the contrast halo is a DARK dilation ring whose whole
        // job is to separate LIGHT text from a BRIGHT additive flame. On a LIGHT
        // theme the text is DARK and the ink-fire is a deep red-brown veil, so a
        // dark ring around dark glyphs only sinks them further into the veil and
        // the flame column swallows the prompt text on the row above.
        // Legibility there comes from keeping the veil translucent,
        // not from a ring — so emit no contrast halo on `Over`.
        for &(er, ec, e) in engulf.iter() {
            if !matches!(mode, FireMode::Add) {
                break;
            }
            // SMOOTH RAMP, not a hard gate: the contrast halo must fade IN with
            // the flame. A hard `e < 0.15` cutoff snaps the ring on at ~25/255
            // the instant the engulfment crosses it, so a glyph at a wobbling
            // flame head flickers between no ring and a visible ring as the
            // envelope breathes around the threshold. A smoothstep over a low toe
            // ramps the strength up from zero — the first ring is a whisper.
            const HALO_TOE: f32 = 0.05;
            let s = ((e - HALO_TOE) / (1.5 - HALO_TOE)).clamp(0.0, 1.0);
            let s = s * s * (3.0 - 2.0 * s);
            let strength = (s * 255.0) as u8;
            if strength == 0 {
                continue;
            }
            halo_cells.push(FireHaloCell {
                row: er,
                col: ec,
                strength,
            });
        }
        // The renderer's FireHaloCell invariant: per-row col-sorted, unique
        // cells. Engulf accumulation merges duplicates; insertion order
        // follows the burning-cell heat order, so sort here (tiny: engulfed
        // cells only) — exactly as the charred stream was sorted.
        halo_cells.sort_unstable_by_key(|c| (c.row, c.col));
    }

    /// Claim one Water cell for this frame's newest-visible-owner pass.
    ///
    /// The table is deliberately independent of grid dimensions: a hostile
    /// geometry may contain billions of logical cells, while resident Water
    /// work is bounded by [`Self::MAX_SPARKS`]. At 50% maximum load, linear
    /// probing is short and deterministic; `0` is free because the encoded
    /// 32-bit coordinate is stored as `key + 1` in a `u64`.
    #[inline]
    pub(super) fn claim_water_cell(seen: &mut [u64], row: u16, col: u16) -> bool {
        debug_assert!(seen.len().is_power_of_two());
        let encoded = (u32::from(row) << 16) | u32::from(col);
        let key = u64::from(encoded) + 1;
        let mask = seen.len() - 1;
        let mut slot = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) as usize) & mask;
        loop {
            match seen[slot] {
                0 => {
                    seen[slot] = key;
                    return true;
                }
                owner if owner == key => return false,
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    /// Emit Water's own fluid wake. Water intentionally does not use the shared
    /// straight comet: its live path samples form a curved two-layer wave, with a
    /// dim deep-blue undertow under a thin cyan crest. A repaint-heavy TUI may
    /// revisit one cell many times, so only its newest visible owner is emitted.
    /// Jump reflections then transfer into sparse falling beads as they fade;
    /// the whole broad mark is under glyph ink, leaving text fully readable.
    pub(super) fn emit_water(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
    ) {
        // Render scratch must describe THIS frame even when the semantic pool
        // just reached idle zero; otherwise diagnostics/tests can observe the
        // previous frame's already-unrendered samples.
        self.water_samples.clear();
        if !matches!(cfg.style, GlowStyle::Water) || self.sparks.is_empty() {
            return;
        }
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let heat = self.heat.clamp(0.0, 1.0);
        // The saturated ocean stops carry less raw luminance than pale
        // ice-cyan. This is a PER-CELL ceiling now: newest-owner dedup below
        // prevents many individually legal claims from stacking past it.
        const WAKE_COV_CAP: f32 = 88.0;

        if self.water_seen.len() == Self::WATER_OWNER_SLOTS {
            self.water_seen.fill(0);
        } else {
            self.water_seen.clear();
            self.water_seen.resize(Self::WATER_OWNER_SLOTS, 0);
        }

        // Newest first: the first visible sample to claim a coordinate owns it.
        // Claim only AFTER coverage is known — a sample rounded fully dark must
        // not hide an older owner that is still visible.
        let mut head: Option<(u8, f32)> = None;
        for s in self.sparks.iter().rev() {
            if (s.row as usize) >= geom.rows || (s.col as usize) >= geom.cols {
                continue;
            }
            // The cursor bridge below is the sole owner of the live caret cell.
            // Excluding stale path owners here prevents an A↔B repaint loop
            // from closing a bright self-intersecting chord at the destination.
            if cur == Some((s.row, s.col)) {
                continue;
            }
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            let u = (age / s.life).clamp(0.0, 1.0);
            // Ease the old tail away faster than the foam at the head.
            let env = (1.0 - u) * (1.0 - 0.45 * u);
            let base_cov = (f32::from(s.born_cov) * env * cfg.intensity * 0.85).min(WAKE_COV_CAP);

            // A stable per-cell deal makes only about one third of a jump's
            // old crest become falling beads. They are derived from age rather
            // than stored particles: no new lifecycle state, RNG, or work while
            // idle. The stagger keeps a whole jump from changing phase at once.
            let mut seed = (u32::from(s.row) << 16) | u32::from(s.col);
            seed ^= seed >> 16;
            seed = seed.wrapping_mul(0x7FEB_352D);
            seed ^= seed >> 15;
            seed = seed.wrapping_mul(0x846C_A68B);
            seed ^= seed >> 16;
            let jitter = f32::from((seed >> 8) as u8) / 255.0;
            let drip_start = 0.20 + 0.18 * jitter;
            let drip_t = ((u - drip_start) / (1.0 - drip_start)).clamp(0.0, 1.0);
            let transfer = if s.typing {
                0.0
            } else {
                smoothstep01((u - drip_start) / 0.28)
            };
            let surface_cov = (base_cov * (1.0 - 0.88 * transfer)) as u8;
            let drip_on = !s.typing && seed.is_multiple_of(3);
            let drip_cov = if drip_on && drip_t > 0.0 {
                let arrive = smoothstep01(drip_t / 0.16);
                (base_cov * 0.54 * arrive * (1.0 - 0.20 * drip_t)) as u8
            } else {
                0
            };
            if surface_cov == 0 && drip_cov == 0 {
                continue;
            }
            if !Self::claim_water_cell(&mut self.water_seen, s.row, s.col) {
                continue;
            }
            // The ROLLING SWELL: visibly undulating even at rest — a flat wake
            // reads as a frozen streak, so the resting amplitude is a real
            // pixel-scale roll, and heat piles it into churning surf.
            let phase = s.col as f32 * 0.82 + s.row as f32 * 0.41 - age * (6.0 + 6.0 * heat);
            let wave = phase.sin() * ch * (0.10 + 0.16 * heat);
            let (sx, sy) = geom.cell_center(s.row, s.col);
            let drift = (f32::from((seed >> 24) as u8) / 255.0 - 0.5) * cw * 0.36;
            let sample = WaterSample {
                surface: BeamVertex {
                    x: sx,
                    y: sy + wave,
                    color: water_ramp(0.58 + 0.30 * s.pos + 0.12 * heat),
                    cov: surface_cov,
                },
                row: s.row,
                col: s.col,
                drip_cov,
                drip_x: sx + drift,
                // Gravity is quadratic: the reflection first beads at the
                // surface, then visibly accelerates down through later rows.
                drip_y: sy + wave + ch * (0.08 + 2.15 * drip_t * drip_t),
                drip_h: ch * (0.08 + 0.30 * drip_t),
            };
            if head.is_none() && surface_cov > 0 {
                head = Some((surface_cov, age));
            }
            self.water_samples.push(sample);
        }
        // Restore path order for adjacent-segment construction. Ownership stays
        // newest-wins because claims were decided before this reversal.
        self.water_samples.reverse();

        // Attach the newest live surface to the current cursor without a bright
        // terminal knot. Matching the head's coverage makes the last segment
        // continuous while the smaller Water crown remains a positional cue.
        if let (Some((row, col)), Some((cov, age))) = (cur, head)
            && (row as usize) < geom.rows
            && (col as usize) < geom.cols
        {
            let phase = col as f32 * 0.82 + row as f32 * 0.41 - age * (6.0 + 6.0 * heat);
            let (hx, hy) = geom.cell_center(row, col);
            self.water_samples.push(WaterSample {
                surface: BeamVertex {
                    x: hx,
                    y: hy + phase.sin() * ch * (0.10 + 0.16 * heat),
                    color: water_ramp(0.80 + 0.12 * heat),
                    cov,
                },
                row,
                col,
                drip_cov: 0,
                drip_x: hx,
                drip_y: hy,
                drip_h: 0.0,
            });
        }

        if self.water_samples.len() == 1 && self.water_samples[0].surface.cov > 0 {
            let v = self.water_samples[0].surface;
            // DPI-proportional too (see the wake thickness below): a single
            // droplet is a cell-scaled speck, not a fixed 3×1 px dot that vanishes
            // at retina. Reproduces the old 3×1 px at the ~16 px reference cell.
            let tw = (cw * 0.4).round().max(2.0) as i32;
            let th = (ch * 0.08).round().max(1.0) as i32;
            push_rect(
                out,
                geom,
                v.x.round() as i32 - tw / 2,
                v.y.round() as i32,
                tw,
                th,
                premul_rgb(v.color, v.cov),
            );
        }

        // A heavier liquid body: the undertow carries real mass and the crest is
        // fuller lip of water, so the wake reads as rolling fluid, not frost.
        // Both scale with cell height; the slightly leaner crest plus under-ink
        // composition avoids the cyan-white slab seen over repainting prompts.
        let under_thickness = ch * (0.28 + 0.13 * heat);
        let crest_thickness = ch * (0.10 + 0.09 * heat);
        // Draw newest segments first, so a pathological geometry sheds the old
        // tail rather than the responsive head when it reaches the upload budget.
        for segment in self.water_samples.windows(2).rev() {
            if out.len() >= Self::MAX_QUADS {
                out.truncate(Self::MAX_QUADS);
                return;
            }
            // A fully faded sample may have opened a hole in the source path.
            // Never bridge that hole with a long straight chord: only adjacent
            // cell samples belong to the same fluid run.
            if segment[0].row.abs_diff(segment[1].row) > 1
                || segment[0].col.abs_diff(segment[1].col) > 1
            {
                continue;
            }
            let surface = [segment[0].surface, segment[1].surface];
            if surface.iter().all(|v| v.cov == 0) {
                continue;
            }
            let undertow = surface.map(|v| BeamVertex {
                color: water_ramp(0.34),
                cov: ((v.cov as f32) * 0.34) as u8,
                ..v
            });
            comet_beam(out, geom.beam_clip(), &undertow, under_thickness, 1, 0.0);
            if out.len() >= Self::MAX_QUADS {
                out.truncate(Self::MAX_QUADS);
                return;
            }
            comet_beam(out, geom.beam_clip(), &surface, crest_thickness, 1, 0.0);
        }

        // The old reflection leaves as individual gravity-driven beads instead
        // of a synchronized horizontal strip. Newest first for load shedding;
        // one small rect per dealt cell keeps this far cheaper than another AA
        // beam pass and `push_rect` splits a falling bead at row boundaries.
        let drip_w = (cw * 0.22).round().max(1.0) as i32;
        for sample in self.water_samples.iter().rev() {
            if sample.drip_cov == 0 {
                continue;
            }
            if out.len() >= Self::MAX_QUADS {
                out.truncate(Self::MAX_QUADS);
                return;
            }
            let drip_h = sample.drip_h.round().max(1.0) as i32;
            push_rect(
                out,
                geom,
                sample.drip_x.round() as i32 - drip_w / 2,
                sample.drip_y.round() as i32 - drip_h / 2,
                drip_w,
                drip_h,
                premul_rgb(water_ramp(0.88), sample.drip_cov),
            );
        }
        out.truncate(Self::MAX_QUADS);
    }

    /// Emit the rainbow kitty JUMP "ZOOM!" streaks: each live [`JumpStreak`] is one
    /// anti-aliased BEAM along the jump vector — [`Self::RAINBOW_JUMP_BANDS`] thin
    /// parallel tubes offset PERPENDICULAR to it, so the stack has a
    /// cross-section rather than an edge — one clean swoosh whether the jump is
    /// horizontal, a wrap to the next line, or a leap across the screen. The tail
    /// RETRACTS toward the landing point over the streak's life (the zoom) while
    /// the whole streak fades; the thickness + brightness scale with the launch
    /// momentum. Additive + capped like the ribbon, rasterized via the shared
    /// [`comet_beam`] (CPU/GPU parity).
    ///
    /// **IT IS A POLYLINE, AND ITS COLOUR IS RESOLVED ON THE ARC** — one vertex
    /// per slab, [`spectrum`] read at each, exactly as the ribbon does it. The
    /// long comment at the emit site records what the two-vertex chained-segment
    /// form it replaces actually painted, and what it cost.
    /// Emit the FIRE METEORS: each is one straight anti-aliased streak of
    /// fire along its true jump vector — a white-hot head at the landing, an
    /// orange core, and a deep-red fringe fading up the tail — whose tail
    /// retracts into the landing over its life (real motion, not a parked
    /// bar). Rasterized through the shared AA `comet_beam`, so it is smooth at
    /// any angle and byte-identical across CPU/GPU. Two layered passes per
    /// meteor: a wide dim aura and a hot tapered core.
    pub(super) fn emit_fire_meteors(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
    ) {
        if !matches!(cfg.style, GlowStyle::Fire) || self.fire_meteors.is_empty() {
            return;
        }
        let chf = geom.ch as f32;
        // Text-safety ceiling: the streak crosses live text for a few hundred
        // ms; it must read as light sweeping OVER the glyphs, never a wash.
        // Sized bright per the owner ("I want more of a meteor streak") but
        // still under a wash: the streak sweeps over glyphs for ~0.3 s and the
        // contrast rim keeps them readable.
        const METEOR_COV_CAP: f32 = 185.0;
        for m in &self.fire_meteors {
            let age = now.saturating_duration_since(m.born).as_secs_f32();
            let u = (age / m.life).clamp(0.0, 1.0);
            let fade = 1.0 - u * u; // hold bright, then drop away
            if fade <= 0.02 {
                continue;
            }
            // The tail retracts into the landing point — the meteor's motion.
            let tx = m.x0 + (m.x1 - m.x0) * u;
            let ty = m.y0 + (m.y1 - m.y0) * u;
            let (dx, dy) = (m.x1 - tx, m.y1 - ty);
            // STEEP-DIVE for a shallow (near-horizontal) hop: a one-row jump
            // (wrap, Ctrl-A on a wrapped line, vim motion) drew a nearly
            // horizontal streak across the INTER-LINE GAP — a stray "fire line
            // between the text lines" (the owner's glitch). Clamp the DRAWN
            // horizontal reach to ≤1.6× the vertical drop so the meteor DIVES
            // into its landing instead of skimming between rows. Only tx/dx (the
            // horizontal draw locals) are shadowed; ty/dy — the vertical drop and
            // its life-based retraction — are untouched, so the arrival
            // choreography, coalescing, and the pinned meteor tests are unchanged.
            let (tx, dx) = if dy.abs() > 0.5 && dx.abs() > 1.6 * dy.abs() {
                let ndx = dx.signum() * 1.6 * dy.abs();
                (m.x1 - ndx, ndx)
            } else {
                (tx, dx)
            };
            if (dx * dx + dy * dy).sqrt() < 1.0 {
                continue;
            }
            let head_cov = (METEOR_COV_CAP * fade * cfg.intensity).clamp(0.0, 255.0);
            if head_cov < 1.0 {
                continue;
            }
            // A NEEDLE, not a torch — a line streak rather than billowy fire:
            // thin core, slim taper, but a visible comet body, never a hairline
            // at same-row nav jumps.
            let core_t = (chf * (0.16 + 0.14 * m.mom)).max(2.5);
            // Wide dim AURA first (under the core): deep ember red.
            let aura = [
                BeamVertex {
                    x: tx,
                    y: ty,
                    color: fire_ramp(0.18),
                    cov: (head_cov * 0.06) as u8,
                },
                BeamVertex {
                    x: m.x1,
                    y: m.y1,
                    color: fire_ramp(0.35),
                    cov: (head_cov * 0.16) as u8,
                },
            ];
            comet_beam(out, geom.beam_clip(), &aura, core_t * 1.5, 2, 0.0);
            if out.len() >= Self::MAX_QUADS {
                out.truncate(Self::MAX_QUADS);
                return;
            }
            // Hot tapered CORE: three chained segments — slim red tail, orange
            // mid, thick near-white head wedge striking the landing.
            const SEG: [(f32, f32, f32, f32, f32); 3] = [
                // (start_t, end_t, thickness ×, coverage ×, ramp position)
                (0.0, 0.45, 0.28, 0.16, 0.28),
                (0.45, 0.80, 0.55, 0.55, 0.55),
                (0.80, 1.0, 1.0, 1.0, 0.88),
            ];
            for &(t0, t1, thick, covx, ramp) in &SEG {
                if out.len() >= Self::MAX_QUADS {
                    return;
                }
                let c0 = (head_cov * covx * 0.55) as u8;
                let c1 = (head_cov * covx) as u8;
                if c1 == 0 {
                    continue;
                }
                let verts = [
                    BeamVertex {
                        x: tx + dx * t0,
                        y: ty + dy * t0,
                        color: fire_ramp(ramp * 0.8),
                        cov: c0,
                    },
                    BeamVertex {
                        x: tx + dx * t1,
                        y: ty + dy * t1,
                        color: fire_ramp(ramp),
                        cov: c1,
                    },
                ];
                comet_beam(
                    out,
                    geom.beam_clip(),
                    &verts,
                    (core_t * thick).max(1.5),
                    1,
                    0.0,
                );
                if out.len() >= Self::MAX_QUADS {
                    out.truncate(Self::MAX_QUADS);
                    return;
                }
            }
        }
    }

    // `roll_jump_nova` STOOD HERE and is deleted (step 5). It drew once per
    // observed jump off the module's `frand` stream and handed one bool to
    // both halves of the gesture, so a 1-in-6 fling came up "supernova" and
    // got a fatter streak, a wider scatter, more stars, a 2.6x flash and a
    // 28-grain glitter shell. Every jump now draws the same mark, which is
    // the only way a landing can be tuned, reviewed or trusted.

    // `spawn_nova_shell` STOOD HERE and is deleted (step 5): 28 glitter
    // grains thrown radially off a supernova landing, at 15 cell-heights a
    // second, so the rare jump had a mark the ordinary one did not. The good
    // idea inside the tier — one continuous stroke where everything else is a
    // population — is now every landing's ring.

    // `emit_rainbow_glide_stars` STOOD HERE and is deleted (step 13). 189 lines
    // drawing chained `RAINBOW_BANDS` segments with a tail→head ramp and a
    // white twinkle head, on their own life, their own cap, their own graded
    // slab stride and their own light-theme arm — which is §3's *"the
    // glide-star streak is the jump streak, fired on a different trigger"*, the
    // THIRD copy of one mark. The glide pushes the ZOOM streak now
    // ([`Self::push_streak_px`]) at the grade its own one-to-six-cell reach
    // earns, so there is one mark to tune, one cap to spend and one law for how
    // a fast cursor draws.

    /// **SEAM POINT 3's LEDGER ARM** (`RAINBOW-KITTY-V2.md` §3.4, §18) — what
    /// the §3.4 pass KEEPS for Rainbow Kitty v2's streams, the halo cap and
    /// §2.3's identity clear, and what it DROPS: the per-pixel bed and
    /// over-ink passes of [`Self::spend_rainbow_budget`].
    ///
    /// v2 is SELF-CAPPED at the emitter — the colour train is held to 118
    /// inside the shoulder span, the white layer to `WHITE_QUAD_CAP`, the
    /// stars to the transient/field ceilings, and the over-ink lift over a
    /// probed glyph cell to the same `rainbow_ink_lift_max` law — so the pass
    /// that would re-derive those ceilings pixel by pixel buys nothing. And it
    /// is not cheap: measured through this seam on the 80-cell Ctrl-E
    /// ping-pong (release, 120 Hz), `spend_rainbow_budget` cost ≈ 500 µs of a
    /// ≈ 530 µs tick — ≈ 440 µs of it the BED pass over the train's ~1.6 k
    /// station quads, which are not the ribbon's cell slabs and so take
    /// `budget_and_claim_bed`'s per-pixel arm, and ≈ 40 µs the companions'
    /// exact tiles over ~1.5 k white-heat quads — against ≈ 20 µs for the
    /// engine itself and 74 µs for v1's whole tick. The owner's standing
    /// order (2026-09-05) is that v2 is never slower than v1, and this arm is
    /// what lets the seam keep it. (A memoised cyan-clear identity used to
    /// cost ≈ 2 µs here as well; it went with the rest of the anti-cyan
    /// machinery on 2026-09-15.)
    /// `tests::a_v2_ping_pong_tick_through_the_seam_is_never_slower_than_v1`
    /// pins it.
    /// The polyline scratch v2 is lent ([`rk::Frame::beams`]): the meteor's
    /// 96 stations and the ring's polyline, with room to spare.
    pub(super) const V2_BEAM_SCRATCH: usize = 512;
    /// The cue sink v2 is lent ([`rk::Frame::cues`]): far above the handful a
    /// landing frame mints (the meteor, five rain glints, the key's own).
    pub(super) const V2_CUE_SCRATCH: usize = 64;

    /// Size the three pixel streams to their caps once (see the emit arm of
    /// seam point 3): [`Self::MAX_QUADS`] for `under` and `out`,
    /// [`Self::MAX_HALOS`] for `halos`. A capacity compare per frame after
    /// the first; `reserve` is exact-or-more, never a shrink.
    pub(super) fn reserve_v2_scratch(
        under: &mut Vec<GlowQuad>,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        if under.capacity() < Self::MAX_QUADS {
            under.reserve(Self::MAX_QUADS - under.len());
        }
        if out.capacity() < Self::MAX_QUADS {
            out.reserve(Self::MAX_QUADS - out.len());
        }
        if halos.capacity() < Self::MAX_HALOS {
            halos.reserve(Self::MAX_HALOS - halos.len());
        }
    }

    /// SEAM POINT 3's cap (§18): the halo stream to [`Self::MAX_HALOS`].
    /// v2's quads are self-capped at the emitter; the §3.4 cyan passes that
    /// used to run here were identity on v2's marks (§19.1, C4) and went
    /// with v1's ledger.
    pub(super) fn cap_and_clear_v2_streams(halos: &mut Vec<RainHalo>) {
        halos.truncate(Self::MAX_HALOS);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "clock + config + geometry + cursor + the two output streams; a param struct would obscure the single internal call site"
    )]
    pub(super) fn emit_crown(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        halos: &mut Vec<RainHalo>,
    ) {
        if cfg.radius <= 0.0 {
            return;
        }
        // `last_move` also feeds movement timing and can be stamped by a move
        // that deliberately declined visual admission.  `crown_until` is the
        // bounded emission arm; without this guard, clearing that arm changed
        // scheduler state but the crown still painted until `last_move` aged
        // out — two stray halos with no live effect owning them.
        if !self.crown_until.is_some_and(|until| now < until) {
            return;
        }
        let Some((cr, cc)) = cur else { return };
        if cr as usize >= geom.rows || cc as usize >= geom.cols {
            return;
        }
        let Some(t0) = self.last_move else { return };
        let age_ms = now.saturating_duration_since(t0).as_millis() as u64;
        // The LAST move's window: 350ms for a typing advance (chains across human
        // inter-key gaps), 200ms for a jump. `.max(1)` guards the Default-zeroed
        // field (unreachable in practice — the window is stamped on every move
        // before `crown_until` arms, and this runs only while a crown is armed).
        let window = self.crown_window_ms.max(1);
        if age_ms >= window {
            return;
        }
        // LIGHT THEMES: the crown is pure additive light — invisible on a
        // white ground, and stacked over the cursor's neighbours it LIFTS
        // freshly typed ink toward the background's luminance. Fire keeps its
        // own theme-aware treatment; every
        // other style skips the crown on light (the veil band + the emitter
        // cursor fill carry the style there).
        if !cfg.dark_theme && !matches!(cfg.style, GlowStyle::Fire) {
            return;
        }
        let fade = 1.0 - age_ms as f32 / window as f32;
        let cw = geom.cw as i32;
        let ch = geom.ch as i32;
        let cx = geom.origin_x as i32 + cc as i32 * cw;
        let cy = geom.origin_y as i32 + cr as i32 * ch;
        // Fire's crown SWELLS with the blaze: the mouth of the fire grows from a
        // candle-glow at rest to a wide roaring halo at full burn.
        let r = if matches!(cfg.style, GlowStyle::Fire) {
            (cfg.radius * geom.ch as f32 * (1.1 + 0.7 * self.fire_t())) as i32
        } else {
            (cfg.radius * geom.ch as f32) as i32
        };
        // The crown's peak coverage + hue per style. It is a RADIAL bloom on the
        // cursor cell (see below), never a stack of concentric rectangles. Laser
        // stays in its own hue (monochrome beam — no white flash).
        let (base_cov, color) = match cfg.style {
            // Laser's crown doubles as the beam's IMPACT FLASH (the blaze-driven
            // boost below), so it carries the hottest base coverage.
            GlowStyle::Laser => (105.0f32, cfg.color),
            // A live flame never holds steady: Fire's crown FLICKERS on a fast
            // phase and burns hotter as the eased temperature climbs, so the
            // cursor itself reads as the mouth of the fire. The colour ceiling
            // stops at 0.85 on the black-body ramp — hot ORANGE-GOLD, never a
            // cream-white that stacks with the beam + bloom into a
            // glyph-swallowing box.
            GlowStyle::Fire => (
                (30.0 + 26.0 * self.fire_t()) * (0.74 + 0.26 * (age_ms as f32 * 0.045).sin()),
                fire_ramp(0.55 + 0.30 * self.fire_t()),
            ),
            GlowStyle::Water => (28.0, water_ramp(0.74)),
            GlowStyle::Phaser | GlowStyle::Sparkle => (50.0, hsv2rgb(self.hue, 0.9, 1.0)),
            // Exhaustiveness only: the classic wake draws its own three-rectangle
            // BOX crown in `classic_wake::ClassicWake::emit_crown` (this radial
            // `emit_crown` runs solely on the built-in emit branch, which the
            // classic style never reaches), so this arm is never emitted live.
            // It carries v0.28's crown values so a future embedder that routed
            // here would still get the salvaged look rather than a stranger's.
            GlowStyle::Classic => (50.0, hsv2rgb(self.hue, 0.9, 1.0)),
            // the rainbow kitty's head crown is a soft white-warm bloom (the "engine" glow the
            // ribbon streams from), so the stripes stay the star of the show.
            // The crown is emitted at `base_cov * 1.9 * fade * intensity *
            // typing_boost` (boost <= 1.2) into `glow_halo`, i.e. OVER the
            // glyph pass, with rx = 1.25cw and ry = 0.85ch — wide enough to
            // land on the just-typed LETTER, not only on the (usually blank)
            // cursor cell — so its base must keep the peak inside
            // `OVER_INK_COV_CAP`: 20 puts it at 20*1.9*1.2 = 45.6.
            // BEAM ships with `radius: 0` from the host (the clean tube owns
            // its look — no bloom crown), so this arm only serves an embedder
            // that opts a crown back in: a tight glow in the tube's own hue.
            // v2 owns the rainbow kitty's head (§7.1) and this emitter never
            // runs while it does; a disengaged rainbow tick is dark, so the
            // arm is exhaustiveness, not a mark.
            GlowStyle::RainbowKitty => return,
            GlowStyle::Beam => (46.0, cfg.color),
            GlowStyle::Lumen => (50.0, cfg.color),
            // The comet's crown is the outer COMA — the faint gas envelope
            // around the nucleus. Slightly whitened off the base hue and kept
            // modest: the nucleus-cursor module draws the bright inner coma, so
            // this halo is ambience, not the ball.
            GlowStyle::Comet => (44.0, lerp_rgb(cfg.color, 0x00FF_FFFF, 0.30)),
            // Exhaustiveness only: the custom interpreter draws its own crown in
            // `emit_custom` (this `emit_crown` runs solely on the built-in emit
            // branch), so this arm is a mono fallback and is never emitted live.
            GlowStyle::Custom => (50.0, cfg.color),
        };
        // The crown follows the TYPING heat: barely-there at rest, full-bright only
        // under sustained fast typing — the steady-state glow around the cursor is
        // what reads as "distracting", so it earns its brightness. LASER also rides
        // the jump FLARE: the landing cell ERUPTS in the beam's own hue the instant
        // a fired beam connects, then cools on the flare tau — the "hit" payoff a
        // heat-only crown (cold right after a jump) could never deliver.
        // FIRE rides it too: the landing crown of a jump must ERUPT with the
        // flare (a heat-only crown is cold right after a jump), then cool.
        let boost = if matches!(cfg.style, GlowStyle::Laser) {
            self.typing_boost().max(1.3 * self.blaze())
        } else if matches!(cfg.style, GlowStyle::Fire) {
            (0.28 + 0.92 * self.fire_t()).max(1.2 * self.fire_t())
        } else {
            self.typing_boost()
        };
        // FIRE: the mouth of the fire is two breathing RADIAL halos (a wide soft
        // skirt under a tight hot heart) centred on the cursor; the shared
        // falloff supplies the roundness a rectangle stack cannot.
        if matches!(cfg.style, GlowStyle::Fire) {
            // The crown's LIGHT hovers ABOVE the glyph line (heat rises — the
            // flame body it illuminates sits from the row top upward), never
            // centred on the text row: these broad halos stacked on the glyph
            // band are THE saturator that clamps the typed line to white and
            // erases letter contrast. Raised, the ambience stays and the line
            // keeps its amber ceiling — the charred ink reads through.
            let (ccx, ccy) = (cx as f32 + cw as f32 * 0.5, cy as f32 - ch as f32 * 0.10);
            let reach = r as f32 + ch as f32 * 0.55;
            let peak = (base_cov * fade * cfg.intensity * boost).min(255.0);
            // DARK THEME: the mouth of the fire is two ADDITIVE radial halos (a
            // wide soft skirt under a tight hot heart). LIGHT THEME: additive
            // light only WASHES a white ground — the broad heart, reaching left
            // over the just-typed row, lifts the trailing glyphs toward the
            // background. The flame body (FireMode::Over) + the streak's
            // source-over veil rails already carry the fire on white, so the
            // additive mouth is dropped there — legibility over ambience.
            if cfg.dark_theme && peak >= 1.0 {
                push_halo(
                    halos,
                    geom,
                    ccx,
                    ccy,
                    reach * 1.9,
                    reach * 1.45,
                    premul_rgb(color, (peak * 0.34) as u8),
                );
                push_halo(
                    halos,
                    geom,
                    ccx,
                    ccy,
                    reach * 1.05,
                    reach * 0.85,
                    premul_rgb(fire_ramp(0.62 + 0.23 * self.fire_t()), (peak * 0.82) as u8),
                );
            }
            // The FORGE DRESSING (molten core / hot rim / ember crawl) must NOT
            // live here: this function returns on the crown's LAST-MOVE window
            // (~350 ms), which would cut the dressing to black while the metal is
            // still hot (release τ 1.4 s) and blink it on/off every keystroke at
            // slow typing. It rides `cursor_temp` from `emit_forge_cursor`
            // instead. The two breathing ambience halos above stay crown-gated —
            // they ARE the mouth-of-fire flare of a fresh move.
            return;
        }
        // EVERY remaining style (phaser, rainbow kitty, sparkle, laser, water, beam,
        // lumen, comet): a compact RADIAL bloom on the cursor cell — the same
        // soft elliptical falloff the fire crown uses (both backends render
        // RainHalo identically) — so it has full punch over the empty cursor cell
        // and near-zero by the neighbouring glyphs' centres, keeping the newest
        // letter's contrast at any cadence. NOT a stack of nested rectangles:
        // concentric hard-edged squares read as debug geometry around the emitter
        // (hairline outlines, posterized laser rings with a detached dim quad
        // below the cursor, stepped lumen/sparkle/water banding) and stack with
        // the band/beam head into a saturated newest glyph.
        let (ccx, ccy) = (cx as f32 + cw as f32 * 0.5, cy as f32 + ch as f32 * 0.5);
        let peak = (base_cov * 1.9 * fade * cfg.intensity * boost).min(210.0);
        if peak >= 1.0 {
            let (core_rx, core_ry) = (cw as f32 * 1.25, ch as f32 * 0.85);
            push_halo(
                halos,
                geom,
                ccx,
                ccy,
                core_rx,
                core_ry,
                premul_rgb(color, peak as u8),
            );
            // LASER: the crown doubles as the beam's IMPACT FLASH. A tight hot
            // HEART over the landing cell — same beam hue (no white flash),
            // riding the same blaze-boosted peak — carries the "hit" punch.
            if matches!(cfg.style, GlowStyle::Laser) {
                push_halo(
                    halos,
                    geom,
                    ccx,
                    ccy,
                    cw as f32 * 0.7,
                    ch as f32 * 0.55,
                    premul_rgb(color, (peak * 0.9).min(230.0) as u8),
                );
            }
        }
    }

    /// The FORGE DRESSING on the cursor cell (Fire only): molten core, hot top
    /// rim, and the edge-ember crawl — the worked-metal look, all riding
    /// `cursor_temp`. Deliberately NOT inside `emit_crown`: the metal cools on
    /// `TEMP_RELEASE_TAU` (1.4 s), far slower than the ~350 ms crown window, so
    /// gating the dressing on the crown HARD-POPS it to black at window expiry
    /// (metal still hot) and BLINKS it on/off every keystroke at slow typing. It
    /// emits every frame the metal is warm (`is_active` keeps the timer armed via
    /// the same `cursor_temp` term) and nothing once it cools below
    /// `FORGE_MIN_TEMP` — idle-zero still holds.
    pub(super) fn emit_forge_cursor(
        &self,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        if !matches!(cfg.style, GlowStyle::Fire) {
            return;
        }
        let temp = self.cursor_temp;
        if temp <= Self::FORGE_MIN_TEMP {
            return;
        }
        let Some((cr, cc)) = cur else { return };
        if cr as usize >= geom.rows || cc as usize >= geom.cols {
            return;
        }
        let cw = geom.cw as i32;
        let ch = geom.ch as i32;
        let cx = geom.origin_x as i32 + cc as i32 * cw;
        let cy = geom.origin_y as i32 + cr as i32 * ch;
        // The crown ambience's anchor (ccy lifted 0.10·ch — heat rises).
        let (ccx, ccy) = (cx as f32 + cw as f32 * 0.5, cy as f32 - ch as f32 * 0.10);
        let tn = ((temp - Self::FORGE_MIN_TEMP) / (1.0 - Self::FORGE_MIN_TEMP)).clamp(0.0, 1.0);
        let phase = self.flame_phase;
        // 1) MOLTEN CORE: a small white-hot pool riding just above centre (heat
        //    rises), swelling and pulsing with the metal.
        let pulse = 0.85 + 0.15 * (phase * 2.3).sin();
        let core_cov = (150.0 * tn * pulse * cfg.intensity).min(255.0);
        if core_cov >= 2.0 {
            push_halo(
                halos,
                geom,
                ccx,
                ccy - ch as f32 * 0.12,
                cw as f32 * (0.30 + 0.14 * tn),
                ch as f32 * (0.20 + 0.10 * tn),
                premul_rgb(fire_ramp((0.72 + 0.20 * tn).min(0.92)), core_cov as u8),
            );
        }
        // 2) HOT TOP RIM: the block's top edge glows hottest — a thin bright
        //    line, the signature of metal heated from within.
        let rim_cov = (90.0 * tn * cfg.intensity).min(255.0);
        if rim_cov >= 2.0 {
            push_rect(
                out,
                geom,
                cx,
                cy,
                cw,
                2,
                premul_rgb(fire_ramp(0.88), rim_cov as u8),
            );
        }
        // 3) EDGE-EMBER CRAWL: above half temperature, two live sparks crawl the
        //    block's perimeter (burning-paper edge) — deterministic in the phase,
        //    so they orbit smoothly.
        if temp > 0.5 {
            for k in 0..2u32 {
                let u = fract(phase * (0.23 + 0.07 * k as f32) + k as f32 * 0.5);
                let per = 2.0 * (cw as f32 + ch as f32);
                let d = u * per;
                let (ex, ey) = if d < cw as f32 {
                    (cx as f32 + d, cy as f32)
                } else if d < cw as f32 + ch as f32 {
                    ((cx + cw) as f32, cy as f32 + (d - cw as f32))
                } else if d < 2.0 * cw as f32 + ch as f32 {
                    (
                        (cx + cw) as f32 - (d - cw as f32 - ch as f32),
                        (cy + ch) as f32,
                    )
                } else {
                    (
                        cx as f32,
                        (cy + ch) as f32 - (d - 2.0 * cw as f32 - ch as f32),
                    )
                };
                let crawl_cov = (130.0 * (temp - 0.5) * 2.0 * cfg.intensity).min(200.0);
                if crawl_cov >= 2.0 {
                    push_halo(
                        halos,
                        geom,
                        ex,
                        ey,
                        3.5,
                        3.5,
                        premul_rgb(fire_ramp(0.85), crawl_cov as u8),
                    );
                }
            }
        }
    }

    pub(super) fn emit_ring(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        let Some(ring) = self.ring else { return };
        let age = now.saturating_duration_since(ring.born).as_secs_f32();
        let t = (age / ring.life).clamp(0.0, 1.0);
        let fade = 1.0 - t;
        let cov = (200.0 * fade * cfg.intensity) as u8;
        if cov == 0 {
            return;
        }
        // The jump's grade: every radius below is this much wider (1.0 for a hop).
        let rf = classic_ring_radius_factor(ring.scale);
        let color = match cfg.style {
            GlowStyle::Laser => cfg.color,
            // The landing SHOCKWAVE glows white-hot at birth and cools as it
            // expands — matched to the jump flare erupting inside it.
            GlowStyle::Fire => fire_ramp(0.95 - 0.35 * t),
            GlowStyle::Water => water_ramp(0.6), // the expanding ripple
            _ => cfg.accent,
        };
        // FIRE: the impact is a round expanding FLASH — a radial halo swelling
        // from the strike point, white-hot at birth cooling to ember, with a
        // dimmer aftershock swelling behind it. (The square outline below serves
        // the non-fire styles only.)
        if matches!(cfg.style, GlowStyle::Fire) {
            let grow = (0.55 + 1.25 * t) * geom.ch as f32 * rf;
            push_halo(
                halos,
                geom,
                ring.cx,
                ring.cy,
                grow * 1.5,
                grow * 1.1,
                premul_rgb(color, cov),
            );
            if t > 0.30 {
                let t2 = (t - 0.30) / 0.70;
                let cov2 = (150.0 * (1.0 - t2) * cfg.intensity) as u8;
                if cov2 > 0 {
                    let g2 = (0.55 + 1.25 * t2) * geom.ch as f32 * rf;
                    push_halo(
                        halos,
                        geom,
                        ring.cx,
                        ring.cy,
                        g2 * 1.5,
                        g2 * 1.1,
                        premul_rgb(fire_ramp(0.45), cov2),
                    );
                }
            }
            return;
        }
        // WATER lands as RIPPLES: flattened ellipse rings spreading across the
        // surface from the splash point, chasing each other outward. The shared
        // expanding SQUARE outline reads as a rigid frame, and nothing about
        // water is square. Centred slightly low (the waterline under the glyph),
        // fading as they roll out.
        if matches!(cfg.style, GlowStyle::Water) {
            let cx = ring.cx;
            let cy = ring.cy + geom.ch as f32 * 0.28;
            let mut ripple = |tt: f32, cov: u8, color: u32| {
                if cov == 0 {
                    return;
                }
                let premul = premul_rgb(color, cov);
                let rx = (0.7 + 1.5 * tt) * geom.ch as f32 * rf;
                let ry = (rx * 0.38).max(2.0);
                let thick = 2.0f32;
                let rslab = ((ry * 0.5) as i32).max(2);
                let mut dy = -(ry as i32);
                while dy < ry as i32 {
                    let h = rslab.min(ry as i32 - dy);
                    let ym = (dy as f32 + h as f32 * 0.5) / ry;
                    let outer = rx * (1.0 - ym * ym).max(0.0).sqrt();
                    let inner_rx = (rx - thick).max(0.0);
                    let inner_ry = ry * (inner_rx / rx);
                    let inner = if inner_ry > 0.5 {
                        let yin = (dy as f32 + h as f32 * 0.5) / inner_ry;
                        inner_rx * (1.0 - yin * yin).max(0.0).sqrt()
                    } else {
                        0.0
                    };
                    let (o, i) = (outer as i32, inner as i32);
                    if o > i {
                        push_rect(out, geom, cx as i32 - o, cy as i32 + dy, o - i, h, premul);
                        push_rect(out, geom, cx as i32 + i, cy as i32 + dy, o - i, h, premul);
                    }
                    dy += h;
                }
            };
            ripple(t, cov, color);
            // The trailing second ripple: rings on water always come in trains.
            if t > 0.30 {
                let t2 = (t - 0.30) / 0.70;
                ripple(
                    t2,
                    (150.0 * (1.0 - t2) * cfg.intensity) as u8,
                    water_ramp(0.45),
                );
            }
            return;
        }
        // LIGHT ARM: the expanding additive
        // square shockwave below is invisible on a white ground, so on a light
        // theme the landing ring inverts to an expanding DARKENED source-over
        // veil ring — a spreading shadow that reads as a shockwave (a contrast
        // INCREASE), the darken twin of the additive outline. The veil hue is
        // the ring colour mixed ~28% toward black (the ribbon-rail recipe), its
        // centre over-alpha faded with the ring life and capped for legibility
        // (`aterm_render::halo_over_cap`) so it greys the surround without ever
        // burying text. Applies to every square-outline style (all were
        // additive-only); fire/water/laser returned above with their own art.
        if !cfg.dark_theme {
            let grow = (0.7 + 1.0 * t) * geom.ch as f32 * rf;
            let veil = lerp_rgb(color, 0x0000_0000, 0.28);
            let cap = ((cov as u32) / 2).clamp(1, 120);
            let veil_capped = (veil & 0x00FF_FFFF) | (cap << 24);
            push_halo_over(
                halos,
                geom,
                ring.cx,
                ring.cy,
                grow,
                grow * 0.85,
                veil_capped,
                255,
            );
            return;
        }
        let premul = premul_rgb(color, cov);
        // Expanding square outline: half-size grows from ~0.6 to ~1.6 cells; emit as
        // top/bottom horizontal bars + left/right vertical bars (push_rect splits the
        // verticals into per-row slabs). Thickness ~2px.
        let s = ((0.6 + 1.0 * t) * geom.ch as f32 * rf) as i32;
        let th = 2i32;
        let cx = ring.cx as i32;
        let cy = ring.cy as i32;
        // top + bottom
        push_rect(out, geom, cx - s, cy - s, 2 * s, th, premul);
        push_rect(out, geom, cx - s, cy + s - th, 2 * s, th, premul);
        // left + right, INSET by the bar thickness at both ends so the four
        // bars PARTITION the outline. Full-height verticals re-covered the
        // th×th corner blocks the horizontals already own, and the stream is
        // additive One/One — every corner composited twice and read as a ~2×
        // lit rivet on the expanding square (the rainbow-comb ownership bug's
        // sibling, found by the 2026-09-01 audit). `2s − 2th > 0` always:
        // s ≥ 0.6·ch ≥ 19 while th = 2.
        push_rect(out, geom, cx - s, cy - s + th, th, 2 * s - 2 * th, premul);
        push_rect(
            out,
            geom,
            cx + s - th,
            cy - s + th,
            th,
            2 * s - 2 * th,
            premul,
        );
        // No trailing SECOND ring here: fire's aftershock rides its radial
        // flash and water's ripple train rides its elliptical branch — both
        // return above, so the square outline stays a single clean ping for
        // the remaining styles.
    }

    pub(super) fn emit_particles(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        let sz = (geom.ch as f32 * 0.18).max(2.0) as i32;
        let water = matches!(cfg.style, GlowStyle::Water);
        'flying: for p in self.particles.iter().rev() {
            if out.len() >= Self::MAX_QUADS {
                break 'flying;
            }
            let age = now.saturating_duration_since(p.born).as_secs_f32();
            let t = (age / p.life).clamp(0.0, 1.0);
            let fade = 1.0 - t;
            // Analytic ballistic position.
            let x = p.x0 + p.vx * age;
            let y = p.y0 + p.vy * age + 0.5 * p.gy * age * age;
            // Sparkles rotate their hue over the flight; embers/droplets keep the
            // birth seed. The colour ramp itself lives in `style_particle_color`,
            // shared with the settings-card demo.
            //
            // DEMAND-DRIVEN, not computed up front: three of the arms below —
            // RainbowKitty, Fire and Beam — draw with their OWN colours
            // (twinkle_rgb / the star tints, `fire_ramp` off the ember's own
            // temperature seed, the stardust tint ladder) and never read this
            // value at all. Evaluating it eagerly cost every rainbow-kitty
            // frame a full `fract` + `hsv2rgb` + `lerp_rgb` per live particle
            // (~20 of each) whose result was dropped on the floor. The ramp is
            // pure, so deferring it into the arms that consume it cannot move a
            // byte; the arms that don't simply never call it.
            let particle_color = || {
                let hue = match cfg.style {
                    // Fire/Water/Comet use `hue` as a per-particle SEED (temperature,
                    // droplet depth, grain size + twinkle phase) — it must not roll.
                    GlowStyle::Fire | GlowStyle::Water | GlowStyle::Comet => p.hue,
                    _ => (p.hue + age * 0.5).fract(),
                };
                style_particle_color(cfg.style, cfg.color, hue, fade)
            };
            // Water droplets support the connected wake rather than competing
            // with it: lower coverage keeps a hot seven-drop burst from becoming
            // a cyan knot at the cursor. Other particle styles keep their punch.
            let base_cov = if water { 112.0 } else { 230.0 };
            // `cov_scale` divides a point-stacked burst's punch (Laser:
            // 1/√burst; exactly 1.0 — bit-exact — everywhere else, see the
            // field doc), the anti-white-blob companion to birth spread.
            let cov = (base_cov * p.cov_scale * fade * cfg.intensity) as u8;
            if cov == 0 {
                continue;
            }
            match cfg.style {
                // LASER: ablation sparks draw as velocity-aligned STREAKS — a short
                // dash trailing each spark's live ballistic motion, white-hot at the
                // leading tip cooling to the pure beam hue behind — rasterized via the
                // shared AA beam so the shower stays smooth at any angle and keeps
                // CPU/GPU byte parity. Dots read as debris; streaks read as sparks
                // grinding off the cut.
                GlowStyle::Laser => {
                    push_velocity_dash(
                        out,
                        geom,
                        x,
                        y,
                        p.vx,
                        p.vy + p.gy * age, // live velocity, not birth velocity
                        0.05,              // seconds of motion the dash trails
                        (geom.ch as f32 * 0.11).max(1.5),
                        cfg.color,
                        particle_color(),
                        cov,
                    );
                    continue;
                }
                // BEAM stardust: each mote is a tiny twinkling 4-point star — most
                // white, some ice-blue, the odd one violet (catching the nebula
                // sleeve) — hanging weightless in the wake. The rainbow kitty star-power
                // pattern, re-cooled for deep space.
                GlowStyle::Beam => {
                    let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                    let scov = ((cov as f32) * twinkle).clamp(0.0, 255.0) as u8;
                    if scov > 0 {
                        let tint = if p.hue < 0.45 {
                            0x00FF_FFFF // starlight white
                        } else if p.hue < 0.85 {
                            0x00CF_ECFF // ice-blue
                        } else {
                            0x00A9_8BFF // bright nebula violet
                        };
                        // THE STARDUST LAW: deep space is DUST with the odd
                        // star — the wake's body is round motes; a dealt
                        // 1-in-8 keeps the 4-point twinkle, FINE. The grain is
                        // priced at the plus's COMPOSITED centre
                        // ([`push_dust_halo`]): the star below it stacks three
                        // additive lays, so one halo at the same `scov` would
                        // make the wake's body 2.35x darker than the accent it
                        // stands in for — a wake of dim smudges around the odd
                        // bright cross, which is a split, not a family.
                        if !star_accent((p.hue * 4096.0) as u32) {
                            let r = dust_r(geom.ch as f32, p.hue) * (0.6 + 0.4 * fade);
                            push_dust_halo(halos, geom, x, y, r, tint, scov);
                            continue;
                        }
                        let arm = star_arm_px(star_arm(geom.ch as f32, STAR_ARM_FINE));
                        let (ix, iy) = (x as i32, y as i32);
                        if !push_twinkle_star(
                            out,
                            geom,
                            ix,
                            iy,
                            arm,
                            scov,
                            false,
                            tint,
                            Self::MAX_QUADS,
                        ) {
                            break 'flying;
                        }
                    }
                    continue;
                }
                // COMET debris: each grain TWINKLES on its own seeded phase — glitter
                // catching the light, not a steady dot. Grain size rides the hue
                // seed (no two match); the brightest instant of a twinkle throws a
                // tiny 4-point GLINT (diffraction spikes off an ice facet). Budget:
                // one body quad plus, at the twinkle's peak only, the shared
                // 4-point star; count-capped at spawn well under fire's shower.
                GlowStyle::Comet => {
                    // THE ONE TWINKLE, on the grain's own seeded phase. The
                    // frequency used to ride the hue seed (7..13 rad/s), which
                    // is the one thing a family of stars must NOT disagree on:
                    // the size seed still varies, the blink rate no longer does.
                    let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                    let gcov = ((cov as f32) * twinkle) as u8;
                    if gcov == 0 {
                        continue;
                    }
                    let d = ((geom.ch as f32 * (0.07 + 0.09 * p.hue)) as i32).max(1);
                    let (ix, iy) = (x as i32, y as i32);
                    push_rect(
                        out,
                        geom,
                        ix - d / 2,
                        iy - d / 2,
                        d,
                        d,
                        premul_rgb(particle_color(), gcov),
                    );
                    // Off-peak the grain is just the grain: no glint fired here
                    // before the deal and none is owed now.
                    if !twinkle_peak(twinkle) {
                        continue;
                    }
                    // THE GLINT'S OWN COVERAGE, once, so the cross below and the
                    // compensation beside it cannot disagree about how bright a
                    // glint is.
                    let glint_cov = (f32::from(gcov) * COMET_GLINT_COV) as u8;
                    if out.len() >= Self::MAX_QUADS {
                        break 'flying;
                    }
                    // THE STARDUST LAW: only a dealt 1-in-16 grain throws the
                    // 4-point glint at its twinkle peak — the rest of the tail
                    // shimmers as the dust it is. The glint IS one of the crosses
                    // the owner asked to thin out (2026-08-09, "many fewer of
                    // those cross sparkles"): `twinkle_peak` opens for ~26% of
                    // every grain's cycle, so before the deal a hot tail was
                    // throwing a plus off a quarter of its debris at any instant
                    // — one of the largest cross populations in the module.
                    if star_accent((p.hue * 4096.0) as u32) {
                        // THE ONE STAR — this glint used to be two hand-rolled
                        // `push_rect` bars, the eleventh star mark in the audit
                        // and the only one that could never grow a nucleus.
                        if !push_twinkle_star(
                            out,
                            geom,
                            ix,
                            iy,
                            star_arm_px(star_arm(geom.ch as f32, STAR_ARM_FINE)),
                            glint_cov,
                            false,
                            0x00FF_FFFF,
                            Self::MAX_QUADS,
                        ) {
                            break 'flying;
                        }
                        continue;
                    }
                    // …AND THE UNDEALT GRAIN IS PAID THE GLINT'S LIGHT.
                    //
                    // Comet is the one arm of the rarity change where NO shape
                    // swap happened: everywhere else a plus became a round mote
                    // and the mote was re-priced at the plus's composited centre
                    // ([`crate::effect_util::STAR_STACK_ADD`],
                    // [`stacked_ink_alpha`]). Here the body was already a flat
                    // square and stayed one, so 15-in-16 grains simply LOST the
                    // glint's extra light at their peak — the exact
                    // "fewer crosses became dimmer" defect the owner ruled out.
                    //
                    // [`push_twinkle_star`] lays three coincident additive marks
                    // at the crossing, so the glint used to put
                    // `STAR_STACK_ADD · glint_cov` of WHITE on the grain's centre
                    // pixel. Pay exactly that, as the one primitive the family
                    // already uses for "the light a plus put in the middle": a
                    // hot core over the skirt, sized like [`push_dust_mote`]'s so
                    // the grain keeps its own edge and only its middle lights up.
                    // White, because the glint was white — a diffraction spike
                    // off ice reads as the highlight, not as more of the grain.
                    let c = ((d + 1) / 2).max(1);
                    let core = premul_rgb(
                        0x00FF_FFFF,
                        (f32::from(glint_cov) * STAR_STACK_ADD).min(255.0) as u8,
                    );
                    push_rect(out, geom, ix - c / 2, iy - c / 2, c, c, core);
                    continue;
                }
                // FIRE: a glowing POINT of light, not a flat pixel — a soft wide halo
                // under a hot inner core, flickering on a seeded phase, wafting
                // sideways on turbulence and shrinking as it cools. The hue seed
                // doubles as the ember's temperature + size (pops big and white-hot,
                // smolder motes small and deep red), so no two embers match.
                GlowStyle::Fire => {
                    // A glowing POINT of round light: ONE radial halo whose
                    // integer elliptical falloff supplies both the hot core and
                    // the soft skirt — never a pair of squares.
                    // VELOCITY STRETCH: the live ballistic motion elongates the
                    // falloff ellipse, so a fast riser draws as a streaking spark
                    // and a drifting smolder as a round mote.
                    let flicker = 0.68
                        + 0.32 * (age * (9.0 + 7.0 * p.hue) + p.hue * std::f32::consts::TAU).sin();
                    let waft = (age * (2.0 + 3.0 * p.hue) + p.hue * std::f32::consts::TAU).sin()
                        * geom.cw as f32
                        * 0.6
                        * t;
                    // EMBER-STACK ceiling: dozens of ember halos overlap along a
                    // hot typed run, and their saturating-add pile is THE
                    // white-out over the text (stream-isolation frames pinned it
                    // — not the comet, not the crown). Scaled down with a tighter
                    // skirt below, each ember still sparkles but the pile keeps
                    // an amber ceiling the charred ink reads through.
                    let mut ccov_f = (cov as f32) * flicker * 0.62;
                    // OCCUPIED-CELL discipline at the HEAD (the phaser's stack-budget
                    // / rainbow-star legibility law, applied to the ember shower): the
                    // dense pile of freshly-born near-white embers clusters exactly
                    // over the 1-2 FRESHEST typed glyph cells and washes them to
                    // cream. An ember hovering over a cell the
                    // per-frame row probe shows as a GLYPH dims hard so those letters
                    // read THROUGH the flame; over the BLANK cursor cell and the risen
                    // airspace above the line it keeps FULL punch (embers rise clear
                    // of the row within a beat, so a spark only dims while it is
                    // actually crossing a letter). Probe lives in the PREV slot at
                    // emit time (see the ribbon/star stars).
                    if self.own_row_glyph_at_px(geom, x, y) {
                        ccov_f *= 0.30;
                    }
                    let ccov = ccov_f as u8;
                    if ccov == 0 {
                        continue;
                    }
                    let cell = geom.ch as f32;
                    // Radii back at full: overlapping ember halos ARE the streak's
                    // glue (shrinking them broke the run into a sprite per letter —
                    // owner). The 0.62 coverage ceiling above keeps the merged run
                    // amber instead of clamping white.
                    let base = (cell * (0.10 + 0.15 * p.hue)).max(2.0) * (0.55 + 0.45 * fade);
                    let vx = p.vx;
                    let vy = p.vy + p.gy * age; // live velocity
                    let rx = base * (1.0 + (vx.abs() / cell).min(1.4) * 0.9);
                    let ry = base * (1.0 + (vy.abs() / cell).min(1.4) * 0.9);
                    let core_color = fire_ramp((0.5 + 0.35 * fade + 0.3 * p.hue).min(1.0));
                    push_halo(
                        halos,
                        geom,
                        x + waft,
                        y,
                        rx * 1.9,
                        ry * 1.9,
                        premul_rgb(core_color, ccov),
                    );
                    continue;
                }
                // SPARKLE: the celestial pour — every grain is a SHAPE from the
                // night sky, chosen by a stable per-particle seed (the birth hue,
                // quantized coarsely so the rolling rainbow tint never flips the
                // shape mid-flight): mostly four-point STARS that twinkle and
                // throw diagonal glints at their brightest instant, a scatter of
                // plain glitter for texture, the occasional pale silver MOON (a
                // crescent of three limbs), and now and then a MINI-COMET — a
                // velocity-aligned rainbow streak (the laser-dash rasterizer,
                // re-dressed). Budget: a small constant per grain (the shared
                // star's arms + nucleus, plus at most four glints), guarded per
                // push and hard-bounded by [`Self::MAX_QUADS`].
                GlowStyle::Sparkle => {
                    let seed = (p.hue * 4096.0) as u32;
                    let (ix, iy) = (x as i32, y as i32);
                    match seed % 8 {
                        // THE FAMILY'S DENOMINATOR REACHES SPARKLE (owner,
                        // 2026-08-09: "many fewer of those cross sparkles").
                        //
                        // This style deals its own grammar out of an eight-way
                        // bucket — dust, star, glitter, moon, mini-comet — so
                        // when [`STAR_ACCENT_DEN`] went 8 → 16 for the whole
                        // kit, the ONE population that spelled its share by
                        // hand kept the exact 1-in-8 the owner had just
                        // rejected. Picking `GlowStyle::Sparkle` therefore
                        // still bought a plus every eighth grain of the pour,
                        // which is the complaint, in a whole cursor style, with
                        // a golden test blessing it as a known residual.
                        //
                        // The buckets stay — the moon and the mini-comet are
                        // this style's grammar and are nobody else's business —
                        // and only the STAR bucket is re-dealt: it keeps the
                        // plus when the family's own [`star_accent`] picks the
                        // grain and joins the dust otherwise. `seed % 16 == 0`
                        // is a strict subset of `seed % 8 == 0`, so the pour's
                        // plus share is exactly 1-in-16 and the half of bucket 0
                        // that loses the cross lands on the dust arm below —
                        // count and brightness unchanged, silhouette dealt.
                        0 if star_accent(seed) => {
                            // Four-point star, twinkling on its own seeded phase.
                            let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                            let scov = ((cov as f32) * twinkle).clamp(0.0, 255.0) as u8;
                            if scov > 0 {
                                // Size rides the seed between two NAMED ratios —
                                // the starfield grain at rest to the family
                                // default — the hero span retired with the
                                // stardust law: one dealt accent per eight
                                // grains, never a fat plus in the pour.
                                let arm = star_arm_px(star_arm(
                                    geom.ch as f32,
                                    STAR_ARM_FINE + (STAR_ARM_STD - STAR_ARM_FINE) * p.hue,
                                ));
                                // Arms via the shared star; the glint stays local —
                                // Sparkle's is WHITE and fires only at the twinkle
                                // peak, unlike the helper's gold glint.
                                if !push_twinkle_star(
                                    out,
                                    geom,
                                    ix,
                                    iy,
                                    arm,
                                    scov,
                                    false,
                                    particle_color(),
                                    Self::MAX_QUADS,
                                ) {
                                    break 'flying;
                                }
                                if twinkle_peak(twinkle) {
                                    // The brightest instant throws diffraction
                                    // glints off the points — at the family's own
                                    // glint offset and dimming.
                                    let d = ((arm as f32 * STAR_GLINT).round() as i32).max(1);
                                    let glint = premul_rgb(
                                        0x00FF_FFFF,
                                        (f32::from(scov) * STAR_GLINT_COV) as u8,
                                    );
                                    for (gx, gy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
                                        if out.len() >= Self::MAX_QUADS {
                                            break 'flying;
                                        }
                                        push_rect(out, geom, ix + gx, iy + gy, 1, 1, glint);
                                    }
                                }
                            }
                            continue;
                        }
                        0..=3 => {
                            // THE STARDUST LAW: the pour's former star lion's
                            // share (buckets 1-3) is round celestial dust — and
                            // so is the half of bucket 0 the family's deal
                            // passed over. Brightness keeps the same twinkle;
                            // the dust still glints, it just stopped spelling
                            // '+'.
                            //
                            // AT THE PLUS'S OWN COMPOSITED LIGHT
                            // ([`push_dust_halo`]). This arm is where bucket 0's
                            // re-deal LANDS, so it is the exact seam where a
                            // rarity fix could — and did — become a brightness
                            // cut: the grain above it stacks three additive lays
                            // on its centre, this one used to lay ONE halo at the
                            // same `scov`, so converting a plus to a grain dimmed
                            // that sparkle 2.35x in the middle. That is the
                            // brightness loss the owner rejected, arriving at a
                            // new emitter by the back door of a rarity change.
                            let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                            let scov = ((cov as f32) * twinkle).clamp(0.0, 255.0) as u8;
                            if scov > 0 {
                                let r = dust_r(geom.ch as f32, p.hue) * (0.6 + 0.4 * fade);
                                push_dust_halo(halos, geom, x, y, r, particle_color(), scov);
                            }
                            continue;
                        }
                        4 | 5 => {
                            // Plain glitter grain — the old square, kept as the
                            // texture between the set pieces.
                            push_rect(
                                out,
                                geom,
                                ix - sz / 2,
                                iy - sz / 2,
                                sz,
                                sz,
                                premul_rgb(particle_color(), cov),
                            );
                            continue;
                        }
                        6 => {
                            // The moon: a pale silver crescent — western limb plus
                            // top and bottom horns opening east.
                            let m = premul_rgb(0x00E6_E6F2, cov);
                            let r = ((geom.ch as f32 * 0.11) as i32).max(2);
                            push_rect(out, geom, ix - r, iy - r, r, 1, m);
                            if out.len() >= Self::MAX_QUADS {
                                break 'flying;
                            }
                            push_rect(out, geom, ix - r, iy - r + 1, 1, 2 * r - 1, m);
                            if out.len() >= Self::MAX_QUADS {
                                break 'flying;
                            }
                            push_rect(out, geom, ix - r, iy + r, r, 1, m);
                            continue;
                        }
                        _ => {
                            // Mini-comet: a short rainbow streak trailing the
                            // grain's live motion, dim tail to bright head.
                            // Bound once: the dash takes the SAME hue at both
                            // ends, so this is one ramp evaluation, not two.
                            let dash = particle_color();
                            push_velocity_dash(
                                out,
                                geom,
                                x,
                                y,
                                p.vx,
                                p.vy + p.gy * age,
                                0.08,
                                (geom.ch as f32 * 0.09).max(1.2),
                                dash,
                                dash,
                                cov,
                            );
                            continue;
                        }
                    }
                }
                // Water: per-droplet size (the hue seed doubles as a size seed) and
                // a vertical STRETCH with the current fall speed — a fast-falling
                // drop draws as a streak, which is what reads as "dripping wet".
                // Rising bubbles (negative fall speed) stay small round beads.
                _ => {
                    let (w, h) = if water {
                        let base = (geom.ch as f32 * (0.055 + 0.065 * p.hue)).max(1.0);
                        let v_down = p.vy + p.gy * age;
                        let stretch = (v_down / geom.ch as f32).clamp(0.0, 1.6);
                        (
                            base.round() as i32,
                            (base * (1.0 + stretch)).round().max(1.0) as i32,
                        )
                    } else {
                        (sz, sz)
                    };
                    push_rect(
                        out,
                        geom,
                        x as i32 - w / 2,
                        y as i32 - h / 2,
                        w,
                        h,
                        premul_rgb(particle_color(), cov),
                    );
                }
            }
        }
    }

    // ----- helpers -----

    /// The comet colour at path position `pos` (0 tail .. 1 head) for the style.
    pub(super) fn comet_color(&self, cfg: &GlowConfig, pos: f32) -> u32 {
        let base = style_comet_color(cfg.style, cfg.color, cfg.accent, self.hue, pos);
        // Torpedo crest: sustained fast typing churns the water wake toward
        // white FOAM at the head. Heat-aware and live-animator-only — the
        // settings demo's pure ramp (no heat) stays the calm baseline.
        if matches!(cfg.style, GlowStyle::Water) && self.heat > 0.0 {
            return lerp_rgb(base, water_ramp(1.0), 0.5 * self.heat * pos);
        }
        // SMOLDER → BLAZE: at rest Fire's wake glows deep ember-orange; the
        // ramp climbs toward its hot head only as the EASED temperature builds
        // — and visibly COOLS as it decays, so every burst ends in dimming
        // embers (the black-body arc, played out in real time). The head is
        // CAPPED at 0.92 on the ramp (hot yellow-white, never the full cream
        // point): a near-white head stacks with crown + bloom over freshly typed
        // glyphs and washes them out. Heat-aware
        // and live-animator-only, like Water's foam crest above: the settings
        // demo's pure `fire_ramp(pos)` stays the full-blaze baseline.
        if matches!(cfg.style, GlowStyle::Fire) {
            return fire_ramp((pos * (0.50 + 0.45 * self.fire_t())).min(0.92));
        }
        base
    }
}
