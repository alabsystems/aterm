// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EMISSION — turning the engine's live state into this frame's quads,
//! halos and patches, style by style.

use super::*;

/// Test-only counter of `emit_custom` invocations; stays 0 across every
/// built-in tick (no built-in path enters the custom interpreter).
#[cfg(test)]
pub(super) static CUSTOM_EMIT_CALLS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

impl CursorGlow {
    // ----- emitting -----

    /// Emit the live lightning bolts: each frozen jagged channel draws as a
    /// wide same-hue corona under a hot core (branches thinner, dimmer, no
    /// white mix), through the shared [`comet_beam`] so strikes stay smooth at
    /// any angle with CPU/GPU parity. Brightness runs a strobe envelope (sharp
    /// attack, dip, restrike, collapse) so it reads as lightning.
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
            // per-bolt shimmer so parallel strikes never sync. The final phase
            // dims gently; the spatial retraction below extinguishes it.
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
            // Burn-out: past the restrike the channel retracts from its origin
            // toward the tip with a glowing edge (20% of length), withdrawing
            // into where it landed.
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
            // Core: white-hot on main strikes (0.62 white ceiling keeps it in
            // the beam's hue family), pure hue on branches.
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

    /// Emit the vapor (steam, smoke and erase poofs) as growing, thinning
    /// radial halos. Steam: pale blue-white, born small and bright, swelling
    /// ~3× as its coverage collapses. Smoke: warm grey, swelling on a gentle
    /// waft, lingering. Poof: neutral grey and brief, the erase puff any style
    /// may shed.
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
        // Steam and smoke are fire's thermal language and render only under
        // it; the style-agnostic erase poof renders everywhere.
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
            // Light themes: additive vapor cannot brighten white, so puffs are
            // source-over veils (straight RGB, falloff as alpha): steam a cool
            // pale mist, smoke a warm grey.
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
                // The erase poof: neutral grey (text going up in smoke, not
                // fire). On light themes a mid-grey source-over veil with
                // steam's stronger peak, which reads by contrast. On dark an
                // additive puff reads by area, so it is born wider (0.34 cell),
                // swells less, and its opacity falls quadratically from a
                // punchier peak: it arrives and dissipates rather than lingering.
                //
                // The peak is a stacking budget: puffs cluster, and three or
                // four overlap near the collapse point. Over a Nord ground the
                // blue channel clips at Σα ≈ 285, so 84 keeps a three-puff stack
                // (≈ 252) under the clip; the cloud's size says how much vanished
                // and the caret stays the brightest thing on the row.
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

    /// Emit the comet body as a smooth line of light rather than one opaque
    /// cell per swept step (which staircases on diagonals): the swept cells
    /// become a polyline that [`comet_beam`] rasterizes as an anti-aliased beam
    /// (a soft wide halo under a hot core). The head run extends to the live
    /// cursor cell so the beam stays attached.
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

        // Build the comet as runs of adjacent swept cells, so a hide/show gap
        // or stale older group never connects across empty space. Each sample
        // carries its faded coverage and path position (tail 0 → head 1). A
        // fully faded tail cell breaks the colour, not the run. The run
        // scratch is reused in place (inner buffers kept); `runs_len` is the
        // live watermark, and entries past it are emptied spares.
        for r in &mut self.comet_runs {
            r.clear();
        }
        let mut runs_len = 0usize;
        self.comet_run.clear();
        let mut prev: Option<(u16, u16)> = None;
        let mut head_cov = 0u8;
        for s in &self.sparks {
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            // Typing sparks hold full brightness for 55% of life then
            // cosine-fade, so the chained streak stays lit across inter-key
            // gaps. Laser typing sparks run the lightning-trail discharge
            // (full power → bleed → lingering dim residual). Jump sparks fade
            // linearly, except laser, whose fired beam holds for 45% of life
            // then burns out: a front sweeps tail→head with a glowing 15% edge,
            // so the ray drains into the impact point. (Sparks of one jump
            // share born+life, so the front is coherent.)
            let frac = (age / s.life).clamp(0.0, 1.0);
            let tf = if s.typing {
                if matches!(cfg.style, GlowStyle::Beam) {
                    // Beam: full power, then one cosine power-down; the rod dims
                    // uniformly while `core_thick` collapses it toward a
                    // hairline, like an emitter switching off.
                    beam_power(frac)
                } else if matches!(cfg.style, GlowStyle::Laser) {
                    // Lightning-trail discharge: full power while fresh, a
                    // cosine bleed to a residual charge that lingers (flickering
                    // under the ripple below) before its own collapse. Only the
                    // freshest cell is bright, so the wake never parks
                    // full-power light over the text.
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
                // A jumped beam is one rod spanning the leap and powers down
                // with the same uniform envelope as the typing tube.
                beam_power(frac)
            } else {
                1.0 - frac
            };
            let mut cov_f = (s.born_cov as f32) * tf * cfg.intensity;
            // Fire legibility cap: the fire comet rides the typed line, and its
            // head stacked with halos and bloom would saturate glyphs to white.
            // Cap the streak's coverage over the occupied glyph cells to the
            // phaser's text-safety ceiling; the bridge onto the empty cursor
            // cell keeps full punch below (FIRE_HEAD_COV_CAP).
            if matches!(cfg.style, GlowStyle::Fire) {
                cov_f = cov_f.min(Self::FIRE_STREAK_COV_CAP);
            }
            // Phaser text-safety ceiling: PHASER_LAYERS overlap to 1.45× in the
            // full-height core, so capping per-sample coverage at 112 keeps the
            // stacked sum (≈162) under saturation and a same-hue glyph visible.
            // Only overdriven configs reach it (default intensity 0.7 stays
            // below).
            if matches!(cfg.style, GlowStyle::Phaser) {
                cov_f = cov_f.min(112.0);
            }
            // Laser scintillation: a ±14% energy ripple travelling head-ward,
            // deterministic in (pos, age). It modulates around full power, so it
            // reads as surging, never flicker toward dark.
            if matches!(cfg.style, GlowStyle::Laser) {
                cov_f *= 0.86 + 0.14 * (s.pos * 9.0 - age * 26.0).sin();
            }
            let cov = cov_f as u8;
            let here = (s.row, s.col);
            if cov == 0 {
                prev = Some(here); // keep continuity so the next cell isn't a false split
                continue;
            }
            let (x, y) = center(s.row, s.col);
            // Phaser samples carry the cell's laid hue as an offset from the
            // live sweep phase (`comet_color` adds `self.hue` back), so each
            // cell keeps the colour it was laid in. Other styles use the
            // tail→head path position.
            let pos = if matches!(cfg.style, GlowStyle::Phaser) {
                (s.hue - self.hue).rem_euclid(1.0)
            } else {
                s.pos
            };
            let sample = CometSample { x, y, cov, pos };
            let (runs, run) = (&mut self.comet_runs, &mut self.comet_run);
            chain_comet_sample(runs, run, &mut runs_len, prev, here, sample);
            prev = Some(here);
            head_cov = cov;
        }
        publish_comet_run(&mut self.comet_runs, &mut self.comet_run, &mut runs_len);
        if runs_len == 0 {
            return;
        }
        // Connect the head run to the live cursor cell (no trailing gap), but
        // only while the cursor is adjacent to the run's head; after a line
        // change or leap a bridge would draw a stray diagonal from the old text.
        // The bridge attaches at `cfg.head_dx` of the cell (0.5 = centre), since
        // a bar cursor sits at the cell's left edge.
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            // The live head run is at `runs_len - 1` (non-zero here), not
            // `last_mut()`, which could return a stale spare.
            && let Some(last) = self.comet_runs.get_mut(runs_len - 1)
            && let Some(head) = last.last().copied()
        {
            let (_, y) = center(cr, cc);
            // Window-absolute, like `center` with `head_dx` in place of 0.5;
            // without `oxf` the head lands `pad` px left of the cursor in a
            // real window and the adjacency test breaks.
            let x = oxf + (cc as f32 + cfg.head_dx) * cwf;
            if (x - head.x).abs() <= cwf * 1.5 && (y - head.y).abs() <= chf * 1.5 {
                // Fire keeps full punch over the empty cursor cell: swept sparks
                // are capped over glyph cells (FIRE_STREAK_COV_CAP), but the
                // bridge has no glyph to bury, so the head returns to
                // FIRE_HEAD_COV_CAP. `comet_beam` ramps 108→168 across the
                // bridge, so the last glyph is not re-washed.
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
                    // Phaser: the bridge is being laid now, so it leads at the
                    // live sweep phase (offset 0.5, a fresh spark's hue). Other
                    // styles: the head of the path ramp.
                    pos: if matches!(cfg.style, GlowStyle::Phaser) {
                        0.5
                    } else {
                        1.0
                    },
                });
            }
        }

        // Light themes: additive light cannot brighten white, and saturating
        // add over fresh glyphs would lift their ink toward the ground. The
        // beam family renders as source-over veils instead: per swept sample,
        // two darkened saturated rails hugging the row's edges. The glyph band
        // stays veil-free (the rails' falloff dies at the row edge), and ink
        // under the skirts only ever darkens.
        if !cfg.dark_theme {
            for run in &self.comet_runs[..runs_len] {
                for s in run {
                    // Peak strong while fresh, culled by push_halo_over's
                    // perceptual floor (<96) about two-thirds into the fade.
                    let peak = (s.cov as f32 * 1.9).min(230.0) as u8;
                    let color = lerp_rgb(self.comet_color(cfg, s.pos), 0x0000_0000, 0.38);
                    // rx spans past the cell so neighbouring veils merge into a
                    // continuous rail.
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

        // Beam power-down: the tube's thickness follows the newest spark's
        // power envelope, holding full girth while live and thinning toward a
        // hairline as it dims.
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
        // Phaser runs a fat core; with its tight PHASER_LAYERS halo it reads as
        // a solid bar of spectrum behind the cursor.
        let core_thick = match cfg.style {
            // Phaser spans most of the row but leaves the cap-height and
            // descender bands clear so letters on it stay legible.
            GlowStyle::Phaser => (chf * 0.66).max(8.0),
            // Fire's core swells with the eased temperature (slim vein at a
            // smolder, a rope at full blaze), thin enough not to bury glyphs.
            GlowStyle::Fire => (chf * (0.13 + 0.09 * self.fire_t())).max(2.0),
            // FAT charged conductor (~2× the shared core): the lightning strikes
            // fork off a thick high-voltage cable of light, not a hairline.
            GlowStyle::Laser => (chf * 0.26).max(3.5),
            // The BEAM tube: a solid rod (~1/3 cell) at full power, collapsing
            // toward a hairline as the light powers down (`beam_pw` above).
            GlowStyle::Beam => (chf * (0.08 + 0.26 * beam_pw)).max(2.0),
            // Sparkle pours a wide rainbow ribbon (~0.45 cell) for the
            // celestial shapes to fall out of; cap-height and descender bands
            // stay clear.
            GlowStyle::Sparkle => (chf * 0.45).max(5.0),
            _ => (chf * 0.13).max(2.0), // crisp thin core
        };
        // Flatten the per-cell Bresenham path to the true straight move so the beam
        // is a clean diagonal, not a stair-stepped polyline of cell centres.
        let straighten = cwf.max(chf) * 0.8;

        // The laser arm's filament polyline scratch, taken with `mem::take`
        // (the loop calls `self.comet_color`, a whole-`self` borrow) after
        // every early return and restored right after the loop.
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
            // The layered-bloom looks live once in `aterm_render`, shared with
            // the render demos. Laser has its own stack (a razor filament in a
            // wide smooth same-hue halo, fine strides); the phaser's
            // near-cell-height core needs the tight PHASER_LAYERS halo.
            match cfg.style {
                GlowStyle::Laser => {
                    // Filament and tight body: the inner laser layers only
                    // (thickness×core ≤ 2.9, ≤ ~0.75 cell) at 1 px strides.
                    // Tuple: (thickness×, coverage×, white-mix base, white-mix ×pos).
                    const LASER_CORE: [(f32, f32, f32, f32); 3] = [
                        (2.9, 0.60, 0.0, 0.0),   // inner glow (pure hue)
                        (1.5, 0.95, 0.10, 0.0),  // beam body
                        (0.6, 1.00, 0.26, 0.42), // white-hot filament, hotter to the head
                    ];
                    // `comet_verts` is cleared at each layer pass and
                    // `comet_beam` reads only the filled prefix.
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
                    // The wide aura is radial halos, never per-row quad slabs,
                    // which would posterize into bands and drop a detached quad
                    // below the cursor. One halo per swept sample; neighbours
                    // merge into one tube in the pure beam hue.
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
                    // No RDP straightening for the phaser: its runs are same-row,
                    // so simplification would only drop interior samples and
                    // their laid hues, collapsing the spectrum to one lerped
                    // colour. Keeping every sample renders each cell in its laid
                    // hue; geometry is identical (collinear samples).
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

    /// The trail-pack interpreter: the emit branch reached only when
    /// `cfg.pack` is `Some` (see [`Self::tick`]). It drives the existing
    /// rasterizers from the pack's [`TrailParams`] and never calls the
    /// per-style `emit_comet`/`emit_crown`/`emit_particles` bodies; no built-in
    /// path reads `cfg.pack`. Every custom sample passes through this
    /// function's funnels, where [`Self::CUSTOM_COV_CAP`] applies.
    ///
    /// `&mut self` only so the beam funnel can reuse the resident
    /// `comet_runs`/`comet_run` scratch (the two arms are exclusive per frame
    /// and both clear on entry).
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
            // Build contiguous runs as `emit_comet` does, but with the pack's
            // fade envelope and every coverage clamped to the structural
            // ceiling; this is the sole beam funnel. The shared run scratch must
            // be cleared in place, or last frame's samples render as a ghost.
            // `runs_len` is the live watermark.
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
                let (x, y) = center(s.row, s.col);
                let sample = CometSample {
                    x,
                    y,
                    cov,
                    pos: s.pos,
                };
                let (runs, run) = (&mut self.comet_runs, &mut self.comet_run);
                chain_comet_sample(runs, run, &mut runs_len, prev, here, sample);
                prev = Some(here);
                head_cov = cov;
            }
            publish_comet_run(&mut self.comet_runs, &mut self.comet_run, &mut runs_len);
            // Bridge the head run to the live cursor cell while adjacent, as
            // `emit_comet` does. Indexing the watermark (not `last_mut()`) keeps
            // a stale pooled spare out of the bridge.
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
            // Light-theme treatment (the pack's `theme` arm): on a light theme an
            // OverVeil/DarkenTints pack renders the beam as source-over veil
            // rails hugging the row's edges, as the built-in beam family does;
            // DarkenTints tints them deeper so a saturated hue survives. `Auto`
            // keeps the additive beam. Dark themes always draw the additive beam.
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
            // Bed channel (`channels.bed`): a faint additive cell-body fill under
            // the beam, clamped well under the structural ceiling so the glyph
            // reads through. Skipped on the light-veil path.
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

        // ---- crown channel (RainHalo) ----
        // A compact radial bloom on the cursor cell, gated on the shared
        // last-move window. Dark theme only, like the built-in non-fire crown.
        if p.crown.enabled
            && cfg.dark_theme
            && matches!(p.channels.halo, HaloChannel::Add | HaloChannel::Over)
            // `last_move` is the glide clock, not an emission licence; the
            // explicit crown arm is authoritative, so clearing it on a denied
            // move darkens this channel.
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

        // ---- ring channel ----
        // Built-in ring geometry from the shared `self.ring` state; its colour
        // match resolves `Custom` to accent.
        if p.ring.enabled {
            self.emit_ring(now, cfg, geom, out, halos);
        }

        // ---- particles ----
        // Pack populations from the shared collection: a dot coloured from the
        // pack ramp, dimmed over an occupied glyph cell, clamped to the ceiling.
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

    /// Per-pixel fire: each hot cell of the envelope emits [`FirePatch`]es
    /// (split per row band, sharing root and phase so the field is seamless)
    /// that both backends evaluate per device pixel through the shared integer
    /// field. Dark themes composite additively; light themes get the ink-fire
    /// ([`FireMode::Over`]). The per-cell engulfment, derived analytically from
    /// the same envelope, feeds the contrast-halo strengths (`halo_cells`), a
    /// colour-free legibility stream (ink is never recoloured).
    #[allow(clippy::too_many_arguments)] // the cool-veil halo sink crossed the arity line
    pub(super) fn emit_flames(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        patches: &mut Vec<FirePatch>,
        // Unused; always left empty (see the contrast-halo note below).
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
        // Legibility-first coverage ceiling: the fire crosses live text, so the
        // field stays a translucent tint the glyph shows through, even at the
        // white-hot head. The drama comes from height, motion and the wall
        // above the line, not opacity over the letters.
        let cov_cap = (22.0 + 12.0 * t).min(255.0) as u8;
        // Per-cell heat envelope: the strongest spark per cell wins, newest
        // first. 12 cells at full strength plus a 4-cell fading tail, so the
        // oldest burning cell dissolves instead of vanishing in one frame.
        let mut cells: [(u16, u16, f32); 16] = [(0, 0, 0.0); 16];
        let mut n_cells = 0usize;
        for s in self.sparks.iter().rev() {
            if (s.row as usize) >= geom.rows || (s.col as usize) >= geom.cols {
                continue;
            }
            // Test cell membership before the transcendentals: a spark whose
            // cell is neither kept nor a free slot is dropped regardless of
            // strength.
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
            // Steep head→tail falloff: flames at the head, embers a few cells
            // back, darkness at the tail.
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
            // Tail dissolve: the first 12 cells keep full strength; the tail
            // fades linearly so the oldest cell drops below the `peak < 1.5`
            // cull as a whisper.
            let strength = if rank < 12 {
                strength
            } else {
                strength * ((cells.len() - rank) as f32 / (cells.len() - 11) as f32).min(1.0)
            };
            // Height ramps quadratically in the eased temperature: a lone peck
            // is a sixth-of-a-cell lick, sustained key-repeat cadence
            // (inter-key ≤ HEAT_GAP_FULL) builds toward a ~6-cell wall. cov_cap
            // keeps even the wall translucent over text.
            let peak = ch * (0.16 + 5.80 * t * t) * strength;
            if peak < 1.5 {
                continue;
            }
            // Root the flame at the cell top so it rises into the inter-line
            // space and the row above, keeping the typed line clear. Both clamps
            // are grid-relative, relaxed by `geom.head` (the chrome band), then
            // translated by origin, never window-clamped, so head == 0 matches
            // the pad-relative math. With head == 0 the root clamps at least
            // one cell down; with head ≥ ch row-0 flames climb into the chrome
            // band, and `fire_field::fire_top_fade` dissolves them at the edge.
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
            // Shift the sampling window in the lean direction rather than
            // widening it both ways. The field shears internally
            // (`shear = lean·vn/4`) and is a function of absolute x, so
            // overlapping windows would add into a bright seam at every cell
            // boundary; a uniform shift makes adjacent patches tile exactly.
            let lean_px = (lean_q as i32).abs() / 4 + 1;
            let shift = if lean_q > 0 { lean_px } else { -lean_px };
            let ox = geom.origin_x as i32;
            let x0 = (ox + (col as i32) * geom.cw as i32 + shift).max(fx_l);
            let x1 = (ox + ((col as i32) + 1) * geom.cw as i32 + shift).min(fx_r);
            if x1 <= x0 {
                continue;
            }
            // Split per row band (the dirty gate stays exact); bands share
            // base_y/phase/params, so the field is seamless. Above-grid bands
            // come out negative and emit with damage-hint row 0, so fire in the
            // chrome band still presents.
            let chi = geom.ch as i32;
            // Root skirt: the field dissolves (mirrored, 5x compressed) below
            // the root through the glyph row's top, so emit those bands too,
            // clipped to the effects box. Under-ink: letters paint over it.
            // (`chi.min(3)` keeps the clamp well-formed on 1 px cells.)
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
                    // Engulfment: how much of this band the flame body covers
                    // (band-height share × envelope × density). Only for
                    // `band >= 0`: above-grid bands have no glyph, and a negative
                    // band must never reach `band as u16`.
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
        // No char, no veil: ink never changes colour (a letter that dims and
        // pops back reads as lag). The engulfment instead feeds the contrast
        // halo, a colour-free strength per engulfed cell driving the dark
        // dilation ring the renderers stamp around glyph strokes (over the
        // flame, under the ink), so letters separate from a bright blaze. It
        // applies on every row the flame covers.
        //
        // Dark theme only: on a light theme text is dark and the ink-fire is a
        // deep veil, so a dark ring would sink glyphs further; legibility there
        // comes from keeping the veil translucent.
        for &(er, ec, e) in engulf.iter() {
            if !matches!(mode, FireMode::Add) {
                break;
            }
            // Smooth ramp, not a hard gate: a hard cutoff would snap the ring on
            // and flicker it as the envelope breathes around the threshold. A
            // smoothstep over a low toe fades it in from zero.
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
        // cells. Accumulation merges duplicates; sort here.
        halo_cells.sort_unstable_by_key(|c| (c.row, c.col));
    }

    /// Claim one water cell for this frame's newest-visible-owner pass.
    ///
    /// The table is independent of grid dimensions: work is bounded by
    /// [`Self::MAX_SPARKS`]. At 50% maximum load linear probing is short and
    /// deterministic; `0` is free because the 32-bit coordinate is stored as
    /// `key + 1` in a `u64`.
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

    /// Emit water's own fluid wake (not the shared straight comet): live path
    /// samples form a curved two-layer wave, a dim deep-blue undertow under a
    /// thin cyan crest. A repaint-heavy TUI may revisit one cell many times, so
    /// only its newest visible owner is emitted. Jump reflections turn into
    /// sparse falling beads as they fade; the mark is under glyph ink.
    pub(super) fn emit_water(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
    ) {
        // Render scratch describes this frame even when the pool just reached
        // idle zero.
        self.water_samples.clear();
        if !matches!(cfg.style, GlowStyle::Water) || self.sparks.is_empty() {
            return;
        }
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let heat = self.heat.clamp(0.0, 1.0);
        // Per-cell ceiling: saturated ocean stops carry less luminance than
        // pale cyan, and newest-owner dedup keeps claims from stacking past it.
        const WAKE_COV_CAP: f32 = 88.0;

        if self.water_seen.len() == Self::WATER_OWNER_SLOTS {
            self.water_seen.fill(0);
        } else {
            self.water_seen.clear();
            self.water_seen.resize(Self::WATER_OWNER_SLOTS, 0);
        }

        // Newest first: the first visible sample to claim a coordinate owns it.
        // Claim only after coverage is known, so a sample rounded dark does not
        // hide a visible older owner.
        let mut head: Option<(u8, f32)> = None;
        for s in self.sparks.iter().rev() {
            if (s.row as usize) >= geom.rows || (s.col as usize) >= geom.cols {
                continue;
            }
            // The cursor bridge below owns the live caret cell; excluding stale
            // path owners prevents an A↔B repaint loop closing a bright chord.
            if cur == Some((s.row, s.col)) {
                continue;
            }
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            let u = (age / s.life).clamp(0.0, 1.0);
            // Ease the old tail away faster than the foam at the head.
            let env = (1.0 - u) * (1.0 - 0.45 * u);
            let base_cov = (f32::from(s.born_cov) * env * cfg.intensity * 0.85).min(WAKE_COV_CAP);

            // A stable per-cell deal turns about a third of a jump's old crest
            // into falling beads, derived from age (no stored particles, RNG or
            // idle work). The stagger keeps a jump from changing phase at once.
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
            // Rolling swell: visibly undulating even at rest, churning into surf
            // with heat.
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

        // Attach the newest live surface to the cursor at the head's coverage,
        // so the last segment is continuous without a bright knot.
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
            // Cell-scaled droplet (about 3×1 px at a ~16 px cell), so it does not
            // vanish at high DPI.
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

        // A heavy liquid body: a massive undertow and a fuller crest, both
        // scaling with cell height; the leaner crest and under-ink composition
        // avoid a cyan-white slab over repainting prompts.
        let under_thickness = ch * (0.28 + 0.13 * heat);
        let crest_thickness = ch * (0.10 + 0.09 * heat);
        // Draw newest segments first, so a pathological geometry sheds the old
        // tail rather than the responsive head when it reaches the upload budget.
        for segment in self.water_samples.windows(2).rev() {
            if out.len() >= Self::MAX_QUADS {
                out.truncate(Self::MAX_QUADS);
                return;
            }
            // Never bridge a hole left by a faded sample with a long chord: only
            // adjacent cell samples belong to one run.
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

        // The old reflection leaves as individual gravity-driven beads. Newest
        // first for load shedding; one small rect per dealt cell (`push_rect`
        // splits a bead at row boundaries).
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

    /// Emit the fire meteors: each is one straight anti-aliased streak of fire
    /// along its jump vector (a white-hot head at the landing, an orange core,
    /// a deep-red fringe up the tail) whose tail retracts into the landing
    /// over its life. Rasterized through the shared `comet_beam`, smooth at any
    /// angle with CPU/GPU parity. Two passes per meteor: a wide dim aura and a
    /// hot tapered core.
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
        // Text-safety ceiling: bright enough to read as a meteor streak, still
        // under a wash; it crosses glyphs for ~0.3 s and the contrast rim keeps
        // them readable.
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
            // Steep dive for a near-horizontal hop: clamp the drawn horizontal
            // reach to ≤1.6× the vertical drop, so a one-row jump dives into its
            // landing instead of skimming the inter-line gap. Only the
            // horizontal draw locals are shadowed; the vertical drop and its
            // retraction are untouched.
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
            // A needle, not a torch: thin core and slim taper, but a visible
            // body even on same-row jumps.
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

    /// Polyline scratch lent to v2 ([`rk::Frame::beams`]): the meteor's 96
    /// stations and the ring's polyline, with room to spare
    /// (owner, 2026-09-05).
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

    /// Cap the halo stream to [`Self::MAX_HALOS`]; v2's quads are self-capped at
    /// the emitter.
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
        // `crown_until` is the bounded emission arm (`last_move` may be stamped
        // by a move that declined visual admission); without this guard a
        // cleared arm would still paint until `last_move` aged out.
        if !self.crown_until.is_some_and(|until| now < until) {
            return;
        }
        let Some((cr, cc)) = cur else { return };
        if cr as usize >= geom.rows || cc as usize >= geom.cols {
            return;
        }
        let Some(t0) = self.last_move else { return };
        let age_ms = now.saturating_duration_since(t0).as_millis() as u64;
        // The last move's window: 350 ms for a typing advance (chains across
        // inter-key gaps), 200 ms for a jump. `.max(1)` guards the zeroed
        // default.
        let window = self.crown_window_ms.max(1);
        if age_ms >= window {
            return;
        }
        // Light themes: the crown is pure additive light, invisible on white
        // and lifting fresh ink toward the ground. Fire keeps its own treatment;
        // other styles skip the crown on light.
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
            // Laser's crown doubles as the beam's impact flash (boosted below),
            // so it carries the hottest base.
            GlowStyle::Laser => (105.0f32, cfg.color),
            // Fire's crown flickers on a fast phase and burns hotter as the
            // eased temperature climbs, capped at 0.85 on the black-body ramp
            // (orange-gold, never a cream-white box).
            GlowStyle::Fire => (
                (30.0 + 26.0 * self.fire_t()) * (0.74 + 0.26 * (age_ms as f32 * 0.045).sin()),
                fire_ramp(0.55 + 0.30 * self.fire_t()),
            ),
            GlowStyle::Water => (28.0, water_ramp(0.74)),
            GlowStyle::Phaser | GlowStyle::Sparkle => (50.0, hsv2rgb(self.hue, 0.9, 1.0)),
            // Exhaustiveness only: the classic wake draws its own box crown in
            // `classic_wake::ClassicWake::emit_crown`.
            GlowStyle::Classic => (50.0, hsv2rgb(self.hue, 0.9, 1.0)),
            // Exhaustiveness only: v2 owns the rainbow kitty's head and this
            // emitter never runs while it does.
            GlowStyle::RainbowKitty => return,
            GlowStyle::Beam => (46.0, cfg.color),
            GlowStyle::Lumen => (50.0, cfg.color),
            // The comet's crown is the faint outer coma, slightly whitened; the
            // nucleus cursor draws the bright inner coma.
            GlowStyle::Comet => (44.0, lerp_rgb(cfg.color, 0x00FF_FFFF, 0.30)),
            // Exhaustiveness only: the custom interpreter draws its own crown in
            // `emit_custom`.
            GlowStyle::Custom => (50.0, cfg.color),
        };
        // The crown follows typing heat: faint at rest, full only under
        // sustained fast typing. Laser and fire also ride the jump flare, so
        // the landing cell erupts the instant a jump connects, then cools on
        // the flare tau.
        let boost = if matches!(cfg.style, GlowStyle::Laser) {
            self.typing_boost().max(1.3 * self.blaze())
        } else if matches!(cfg.style, GlowStyle::Fire) {
            (0.28 + 0.92 * self.fire_t()).max(1.2 * self.fire_t())
        } else {
            self.typing_boost()
        };
        // Fire: the mouth of the fire is two breathing radial halos (a wide
        // soft skirt under a tight hot heart) on the cursor.
        if matches!(cfg.style, GlowStyle::Fire) {
            // The light hovers above the glyph line (heat rises); broad halos
            // centred on the text row would clamp the typed line to white.
            let (ccx, ccy) = (cx as f32 + cw as f32 * 0.5, cy as f32 - ch as f32 * 0.10);
            let reach = r as f32 + ch as f32 * 0.55;
            let peak = (base_cov * fade * cfg.intensity * boost).min(255.0);
            // Dark theme: two additive halos. Light theme: additive light only
            // washes a white ground, and the flame body (`FireMode::Over`) plus
            // the streak's veil rails already carry the fire, so the mouth is
            // dropped there.
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
            // The forge dressing does not live here: this function returns on
            // the ~350 ms last-move window, which would black it out while the
            // metal is still hot (release τ 1.4 s). It rides `cursor_temp` in
            // `emit_forge_cursor`; the ambience halos above stay crown-gated.
            return;
        }
        // Every remaining style (phaser, rainbow kitty, sparkle, laser, water,
        // beam, lumen, comet): a compact radial bloom on the cursor cell, full
        // punch over the empty cell and near zero at the neighbouring glyphs'
        // centres, so the newest letter keeps contrast. Not concentric
        // rectangles, which read as debug geometry and saturate the newest
        // glyph.
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
            // Laser: a tight hot heart over the landing cell in the beam hue,
            // riding the blaze-boosted peak, as the impact flash.
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

    /// The forge dressing on the cursor cell (fire only): molten core, hot
    /// top rim and edge-ember crawl, all riding `cursor_temp`. Not inside
    /// `emit_crown`: the metal cools on `TEMP_RELEASE_TAU` (1.4 s), far slower
    /// than the ~350 ms crown window. Emits every frame the metal is warm
    /// (`is_active` keeps the timer armed) and nothing below `FORGE_MIN_TEMP`.
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
        // Fire: the impact is a round expanding flash (white-hot cooling to
        // ember) with a dimmer aftershock behind it.
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
        // Water lands as ripples: flattened ellipse rings spreading from the
        // splash point, centred slightly low (the waterline), fading outward.
        if matches!(cfg.style, GlowStyle::Water) {
            let cx = ring.cx;
            let cy = ring.cy + geom.ch as f32 * 0.28;
            let mut ripple = |tt: f32, cov: u8, color: u32| {
                if cov == 0 {
                    return;
                }
                let rx = (0.7 + 1.5 * tt) * geom.ch as f32 * rf;
                let ry = (rx * 0.38).max(2.0);
                push_ripple_ring(out, geom, (cx, cy), (rx, ry), 2.0, premul_rgb(color, cov));
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
        // Light arm: an additive square shockwave is invisible on white, so the
        // ring inverts to an expanding darkened source-over veil ring. The hue
        // is the ring colour mixed ~28% toward black; its centre over-alpha
        // fades with the ring life, capped by `aterm_render::halo_over_cap` so
        // it never buries text. Fire, water and laser returned above.
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
        // Expanding square outline: half-size grows from ~0.6 to ~1.6 cells,
        // emitted as four ~2 px bars (push_rect splits verticals per row).
        let s = ((0.6 + 1.0 * t) * geom.ch as f32 * rf) as i32;
        let th = 2i32;
        let cx = ring.cx as i32;
        let cy = ring.cy as i32;
        // top + bottom
        push_rect(out, geom, cx - s, cy - s, 2 * s, th, premul);
        push_rect(out, geom, cx - s, cy + s - th, 2 * s, th, premul);
        // Left and right bars are inset by the bar thickness so the four bars
        // partition the outline; the stream is additive, so a corner covered
        // twice would read as a bright rivet. `2s − 2th > 0` always
        // (s ≥ 0.6·ch ≥ 19, th = 2).
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
        // A single ring: fire's aftershock and water's ripple train returned
        // above.
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
            // Sparkles rotate hue over the flight; embers and droplets keep the
            // birth seed. The ramp (`style_particle_color`, shared with the
            // settings demo) is evaluated lazily: the rainbow kitty, fire and
            // beam arms use their own colours.
            let particle_color = || {
                let hue = match cfg.style {
                    // Fire/Water/Comet use `hue` as a per-particle SEED (temperature,
                    // droplet depth, grain size + twinkle phase) — it must not roll.
                    GlowStyle::Fire | GlowStyle::Water | GlowStyle::Comet => p.hue,
                    _ => (p.hue + age * 0.5).fract(),
                };
                style_particle_color(cfg.style, cfg.color, hue, fade)
            };
            // Water droplets support the wake: lower coverage keeps a burst from
            // becoming a cyan knot at the cursor.
            let base_cov = if water { 112.0 } else { 230.0 };
            // `cov_scale` divides a point-stacked burst's punch (laser:
            // 1/√burst; exactly 1.0 elsewhere).
            let cov = (base_cov * p.cov_scale * fade * cfg.intensity) as u8;
            if cov == 0 {
                continue;
            }
            match cfg.style {
                // Laser: ablation sparks draw as velocity-aligned streaks (white
                // tip cooling to the beam hue) via the shared AA beam.
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
                // Beam stardust: tiny twinkling 4-point stars, mostly white, some
                // ice-blue, the odd violet, hanging in the wake.
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
                        // Stardust rule: the wake's body is round motes and a
                        // dealt 1-in-8 keeps the 4-point twinkle. The mote is
                        // priced at the plus's composited centre
                        // ([`push_dust_halo`]) so the body matches the accent's
                        // brightness.
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
                // Comet debris: each grain twinkles on its own seeded phase,
                // size riding the hue seed; a dealt grain throws a tiny 4-point
                // glint at its twinkle peak.
                GlowStyle::Comet => {
                    // One twinkle rate for the family; only the size seed varies.
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
                    // The glint's coverage, shared by the cross and its
                    // compensation.
                    let glint_cov = (f32::from(gcov) * COMET_GLINT_COV) as u8;
                    if out.len() >= Self::MAX_QUADS {
                        break 'flying;
                    }
                    // Only a dealt 1-in-16 grain throws the glint at its twinkle
                    // peak; the rest shimmer as dust (owner, 2026-08-09).
                    if star_accent((p.hue * 4096.0) as u32) {
                        // The shared star mark.
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
                    // The undealt grain is paid the glint's light:
                    // [`push_twinkle_star`] lays three coincident additive marks,
                    // putting `STAR_STACK_ADD · glint_cov` of white on the
                    // centre. Pay that as a hot white core over the square, sized
                    // like [`push_dust_mote`]'s, so fewer crosses do not mean
                    // dimmer grains.
                    let c = ((d + 1) / 2).max(1);
                    let core = premul_rgb(
                        0x00FF_FFFF,
                        (f32::from(glint_cov) * STAR_STACK_ADD).min(255.0) as u8,
                    );
                    push_rect(out, geom, ix - c / 2, iy - c / 2, c, c, core);
                    continue;
                }
                // Fire: a glowing point of light flickering on a seeded phase,
                // wafting on turbulence and shrinking as it cools. The hue seed
                // doubles as temperature and size (pops big and white-hot,
                // smolder motes small and deep red).
                GlowStyle::Fire => {
                    // One radial halo supplies core and skirt. Velocity stretch
                    // elongates the ellipse, so a fast riser streaks and a
                    // drifting smolder stays round.
                    let flicker = 0.68
                        + 0.32 * (age * (9.0 + 7.0 * p.hue) + p.hue * std::f32::consts::TAU).sin();
                    let waft = (age * (2.0 + 3.0 * p.hue) + p.hue * std::f32::consts::TAU).sin()
                        * geom.cw as f32
                        * 0.6
                        * t;
                    // Ember-stack ceiling: dozens of ember halos overlap along a
                    // hot run and their saturating sum whites out the text;
                    // scaled down, the pile keeps an amber ceiling.
                    let mut ccov_f = (cov as f32) * flicker * 0.62;
                    // Occupied-cell discipline: fresh near-white embers cluster
                    // over the freshest glyphs. An ember over a cell the row probe
                    // shows as a glyph dims hard; over the blank cursor cell and
                    // the airspace above it keeps full punch. The probe lives in
                    // the prev slot at emit time.
                    if self.own_row_glyph_at_px(geom, x, y) {
                        ccov_f *= 0.30;
                    }
                    let ccov = ccov_f as u8;
                    if ccov == 0 {
                        continue;
                    }
                    let cell = geom.ch as f32;
                    // Full radii: overlapping ember halos glue the streak
                    // together; the 0.62 ceiling above keeps it amber.
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
                // Sparkle: every grain is a night-sky shape chosen by a stable
                // per-particle seed (the birth hue, quantized so the rolling tint
                // never flips it): mostly four-point stars that twinkle and glint
                // at their peak, plain glitter, the odd pale moon crescent, and
                // the occasional rainbow mini-comet. A small constant per grain,
                // bounded by [`Self::MAX_QUADS`].
                GlowStyle::Sparkle => {
                    let seed = (p.hue * 4096.0) as u32;
                    let (ix, iy) = (x as i32, y as i32);
                    match seed % 8 {
                        // The star bucket is re-dealt through the family's
                        // [`star_accent`]: `seed % 16 == 0` is a strict subset of
                        // `seed % 8 == 0`, so the pour's plus share is 1-in-16
                        // ([`STAR_ACCENT_DEN`]) and the rest of bucket 0 joins
                        // the dust arm with count and brightness unchanged.
                        0 if star_accent(seed) => {
                            // Four-point star, twinkling on its own seeded phase.
                            let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                            let scov = ((cov as f32) * twinkle).clamp(0.0, 255.0) as u8;
                            if scov > 0 {
                                // Size rides the seed between the starfield grain
                                // at rest and the family default.
                                let arm = star_arm_px(star_arm(
                                    geom.ch as f32,
                                    STAR_ARM_FINE + (STAR_ARM_STD - STAR_ARM_FINE) * p.hue,
                                ));
                                // Arms via the shared star; Sparkle's glint stays
                                // local (white, only at the twinkle peak).
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
                                    // Diffraction glints at the brightest instant,
                                    // at the family's glint offset and dimming.
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
                            // Stardust: buckets 1-3 and the undealt half of bucket
                            // 0 are round celestial dust with the same twinkle,
                            // drawn at the plus's composited light
                            // ([`push_dust_halo`]) so a re-dealt grain is not
                            // dimmer than the plus it replaces.
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
                            // Mini-comet: a short rainbow streak along the
                            // grain's motion; one hue at both ends.
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
                // Water: per-droplet size from the hue seed and a vertical
                // stretch with fall speed (a fast drop streaks); rising bubbles
                // stay small round beads.
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
        // Torpedo crest: sustained fast typing churns the head toward white
        // foam. Live animator only; the settings demo stays calm.
        if matches!(cfg.style, GlowStyle::Water) && self.heat > 0.0 {
            return lerp_rgb(base, water_ramp(1.0), 0.5 * self.heat * pos);
        }
        // Smolder → blaze: at rest the wake glows deep ember-orange, climbing
        // toward the hot head as the eased temperature builds and cooling as it
        // decays. The head is capped at 0.92 on the ramp (never cream), so it
        // does not wash fresh glyphs. Live animator only.
        if matches!(cfg.style, GlowStyle::Fire) {
            return fire_ramp((pos * (0.50 + 0.45 * self.fire_t())).min(0.92));
        }
        base
    }
}

/// Chain one lit swept cell `here` into the comet RUNS (the resident
/// `comet_runs` spine up to its watermark `runs_len`, `run` the open one): a
/// cell more than one step from the previous one, `prev`, publishes the open
/// run first, so a hide/show gap or a stale older group never connects across
/// empty space.
fn chain_comet_sample(
    runs: &mut Vec<Vec<CometSample>>,
    run: &mut Vec<CometSample>,
    runs_len: &mut usize,
    prev: Option<(u16, u16)>,
    here: (u16, u16),
    sample: CometSample,
) {
    if let Some((pr, pc)) = prev
        && (here.0 as i32 - pr as i32)
            .abs()
            .max((here.1 as i32 - pc as i32).abs())
            > 1
    {
        publish_comet_run(runs, run, runs_len);
    }
    run.push(sample);
}

/// Publish the open comet `run`, when it holds a sample, at the pool
/// watermark `runs_len`. Swapped rather than taken, so the next run reuses
/// the pooled buffer's capacity.
fn publish_comet_run(
    runs: &mut Vec<Vec<CometSample>>,
    run: &mut Vec<CometSample>,
    runs_len: &mut usize,
) {
    if run.is_empty() {
        return;
    }
    if *runs_len == runs.len() {
        runs.push(Vec::new());
    }
    std::mem::swap(&mut runs[*runs_len], run);
    *runs_len += 1;
}
