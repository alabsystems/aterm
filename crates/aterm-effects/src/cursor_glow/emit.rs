// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Emission of per-frame cursor-glow quads, halos, and patches by style.

use super::*;

impl CursorGlow {
    /// Emit the live lightning bolts through [`comet_beam`].
    pub(super) fn emit_bolts(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
        verts: &mut Vec<BeamVertex>,
        channel: &mut Vec<(f32, f32, f32, bool)>,
    ) {
        if !matches!(cfg.style, GlowStyle::Laser) || self.bolts.is_empty() {
            return;
        }
        let ch = geom.ch as f32;
        let dark = cfg.dark_theme;
        let light_ink = match InkRole::Leading.ink(cfg.color) {
            0 => InkRole::Leading.ink(0x00FF_FFFF),
            ink => ink,
        };
        let alpha_cap = Self::LIGHT_BOLT_ALPHA_CAP;
        let mut crackle_cap: Option<f32> = None;
        let lift_cap = || {
            let lift = [16u32, 8, 0]
                .map(|sh| ((cfg.color >> sh) & 0xff) as f32 * (1.0 + 1.0 / 3.0) / 255.0);
            Self::over_ink_ceiling(lift)
        };
        let text = self.text_view();
        let over_text = |x: f32, y: f32| {
            let row = ((y - f32::from(geom.origin_y)) / ch).floor() as i32;
            let col = ((x - f32::from(geom.origin_x)) / geom.cw as f32).floor() as i32;
            text.covers_cell(row, col)
        };
        let hold_over_text = dark && !self.streak_unbudgeted();
        let step = (geom.cw as f32 * 0.5).max(1.0);
        let clip = geom.beam_clip();
        for b in self.bolts.iter().rev() {
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            let u = (now.saturating_duration_since(b.born).as_secs_f32() / b.life).clamp(0.0, 1.0);
            let env = bolt_flash(u);
            let mut cov_f = 255.0 * env * cfg.intensity * if b.branch { 0.72 } else { 1.0 };
            if b.crackle && dark && !self.streak_unbudgeted() {
                let cap = *crackle_cap.get_or_insert_with(lift_cap);
                cov_f = cov_f.min(cap);
            }
            if !dark {
                cov_f = cov_f.min(alpha_cap);
            }
            if cov_f < 1.0 {
                continue;
            }
            let cov = cov_f.min(255.0) as u8;
            let k_burn = if u > BOLT_BURN_START {
                (u - BOLT_BURN_START) / (1.0 - BOLT_BURN_START)
            } else {
                0.0
            };
            let n1 = (b.pts.len() - 1).max(1) as f32;
            let burn = |p: f32, c: u8| -> u8 {
                ((c as f32) * ((p - k_burn) / 0.20).clamp(0.0, 1.0)) as u8
            };
            channel.clear();
            let held = hold_over_text && !b.crackle;
            let resample = dark && !b.crackle;
            for (i, w) in b.pts.windows(2).enumerate() {
                let ((x0, y0), (x1, y1)) = (w[0], w[1]);
                let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
                let n = if resample {
                    (len / step).ceil().max(1.0) as usize
                } else {
                    1
                };
                for k in 0..n {
                    let f = k as f32 / n as f32;
                    let (x, y) = (x0 + (x1 - x0) * f, y0 + (y1 - y0) * f);
                    channel.push((x, y, (i as f32 + f) / n1, held && over_text(x, y)));
                }
            }
            if let Some(&(x, y)) = b.pts.last() {
                channel.push((x, y, 1.0, held && over_text(x, y)));
            }
            let text_cap = if channel.iter().any(|c| c.3) {
                *crackle_cap.get_or_insert_with(lift_cap)
            } else {
                255.0
            };
            let mut stroke = |out: &mut Vec<GlowQuad>,
                              color: &dyn Fn(bool) -> u32,
                              cov: u8,
                              share: f32,
                              thick: f32,
                              step| {
                verts.clear();
                for &(x, y, p, over) in channel.iter() {
                    let c = if over {
                        (text_cap * share).min(f32::from(cov)) as u8
                    } else {
                        cov
                    };
                    verts.push(BeamVertex {
                        x,
                        y,
                        color: color(over),
                        cov: burn(p, c),
                    });
                }
                if dark {
                    comet_beam(out, clip, verts, thick, step, 0.0);
                } else {
                    comet_beam_over(out, clip, verts, thick, step, 0.0);
                }
            };
            let corona_th = ch * if b.branch { 0.30 } else { 0.55 };
            let corona = if dark { cfg.color } else { light_ink };
            stroke(out, &|_| corona, (cov / 3).max(1), 1.0 / 3.0, corona_th, 2);
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            let core_color = if !dark {
                light_ink
            } else if b.branch {
                cfg.color
            } else {
                lerp_rgb(cfg.color, 0x00FF_FFFF, 0.62 * env)
            };
            let core_th = (ch * if b.branch { 0.10 } else { 0.18 }).max(1.5);
            let core = |over: bool| if over { cfg.color } else { core_color };
            stroke(out, &core, cov, 1.0, core_th, 1);
        }
    }

    /// Emit the vapor (steam, smoke and erase poofs) as growing, thinning radial halos.
    pub(super) fn emit_vapor(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        halos: &mut Vec<RainHalo>,
    ) {
        if self.vapor.is_empty() {
            return;
        }
        let fire = matches!(cfg.style, GlowStyle::Fire);
        let cell = geom.ch as f32;
        let oy = f32::from(geom.origin_y);
        let budgeted = !self.streak_unbudgeted();
        let steam_share = if fire && budgeted {
            let live: f32 = self
                .vapor
                .iter()
                .filter(|v| v.kind == VaporKind::Steam)
                .map(|v| {
                    let t = (now.saturating_duration_since(v.born).as_secs_f32() / v.life)
                        .clamp(0.0, 1.0);
                    (1.0 - t) * (1.0 - t)
                })
                .sum();
            (Self::STEAM_STACK / live.max(f32::EPSILON)).min(1.0)
        } else {
            1.0
        };
        let text = TextView::new(
            if fire {
                self.probed_text_spans()
            } else {
                TextSpans::default()
            },
            Some(&self.frame_text),
        );
        let covers_text = |x: f32, y: f32| {
            let row = ((y - oy) / cell).floor() as i32;
            let col = ((x - f32::from(geom.origin_x)) / geom.cw as f32).floor() as i32;
            text.covers_cell(row, col)
        };
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
            if v.kind == VaporKind::Smoke && budgeted && covers_text(x, y) {
                continue;
            }
            let (r0, grow, color, peak) = match v.kind {
                VaporKind::Steam => {
                    let row_top = oy + ((v.y0 - oy) / cell).floor() * cell;
                    let climb = ((row_top - y) / (0.6 * cell)).clamp(0.0, 1.0);
                    let rise_fade = if budgeted {
                        1.0 - Self::STEAM_ABOVE_DIM * climb * climb * (3.0 - 2.0 * climb)
                    } else {
                        1.0
                    };
                    (
                        cell * 0.26,
                        1.0 + 3.4 * t,
                        if cfg.dark_theme {
                            0x00C9_D6DE
                        } else {
                            0x00AB_BCC6
                        },
                        (150.0 * (1.0 - t) * (1.0 - t)) * cfg.intensity * steam_share * rise_fade,
                    )
                }
                VaporKind::Smoke => (
                    cell * 0.15,
                    1.0 + 2.4 * t,
                    if cfg.dark_theme {
                        lerp_rgb(0x005E_5E60, 0x0042_4246, t)
                    } else {
                        lerp_rgb(0x0060_544A, 0x0084_7E78, t)
                    },
                    (if cfg.dark_theme { 34.0 } else { 52.0 }) * (1.0 - t) * cfg.intensity,
                ),
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
            if peak < 1.0 {
                continue;
            }
            let r = r0 * grow;
            if cfg.dark_theme {
                let share = if matches!(cfg.style, GlowStyle::Fire) {
                    self.ink_ledger.admit_halo(
                        (x, y),
                        (r * 1.35, r),
                        premul_rgb(color, peak as u8),
                        InkLedger::FLOOR,
                    )
                } else {
                    1.0
                };
                let peak = (peak * share) as u8;
                if peak == 0 {
                    continue;
                }
                push_halo(halos, geom, x, y, r * 1.35, r, premul_rgb(color, peak));
            } else {
                let peak = peak as u8;
                push_halo_over(halos, geom, x, y, r * 1.35, r, color, peak);
            }
        }
    }

    /// Emit the comet body as an anti-aliased beam through [`comet_beam`].
    #[allow(
        clippy::too_many_arguments,
        reason = "clock + config + geometry + cursor + the three streams a beam can land in"
    )]
    pub(super) fn emit_comet(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
        under: &mut Vec<GlowQuad>,
    ) {
        if !cfg.beam || self.sparks.is_empty() {
            return;
        }
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let oxf = geom.origin_x as f32;
        let center = |row: u16, col: u16| geom.cell_center(row, col);

        for r in &mut self.comet_runs {
            r.clear();
        }
        for r in &mut self.comet_typing {
            r.clear();
        }
        let mut runs_len = 0usize;
        self.comet_run.clear();
        self.comet_run_typing.clear();
        let mut prev: Option<(u16, u16)> = None;
        let mut head_full = 0u8;
        let stack = streak_layers(cfg.style);
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
        let bed = if cfg.dark_theme && !self.streak_unbudgeted() {
            BedShape::of(cfg.style, beam_pw).map(|shape| {
                let leapt = self.sparks.last().is_some_and(|s| !s.typing);
                if matches!(cfg.style, GlowStyle::Sparkle) && leapt {
                    let k = cfg.length as f32 / GLOW_LENGTH_DEFAULT as f32;
                    BedShape {
                        wedge: (shape.wedge * k).clamp(1.0, 4.0 * shape.wedge),
                        ..shape
                    }
                } else {
                    shape
                }
            })
        } else {
            None
        };
        #[cfg(test)]
        let bed = bed.map(|shape| BedShape {
            leap_air: shape.leap_air && !self.leap_split,
            ..shape
        });
        let mut fire_lift_memo: Option<((u32, u32), BandLifts)> = None;
        let fire_core = (chf * (0.13 + 0.09 * self.fire_t())).max(2.0);
        let fire_text = if matches!(cfg.style, GlowStyle::Fire)
            && cfg.dark_theme
            && !self.streak_unbudgeted()
        {
            let spans = self.probed_text_spans();
            Some(TextView::new(spans, Some(&self.frame_text)))
        } else {
            None
        };
        for s in &self.sparks {
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            let frac = (age / s.life).clamp(0.0, 1.0);
            let tf = if s.typing {
                if matches!(cfg.style, GlowStyle::Beam) {
                    beam_power(frac)
                } else if matches!(cfg.style, GlowStyle::Laser) {
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
                    let k = (frac - 0.45) / 0.55;
                    ((s.pos - k) / 0.15).clamp(0.0, 1.0) * (1.0 - 0.35 * k)
                }
            } else if matches!(cfg.style, GlowStyle::Beam) {
                beam_power(frac)
            } else {
                1.0 - frac
            };
            let pos = if matches!(cfg.style, GlowStyle::Phaser) {
                (s.hue - self.hue).rem_euclid(1.0)
            } else {
                s.pos
            };
            let peak_f = (s.born_cov as f32) * cfg.intensity;
            let tf = if !cfg.dark_theme && matches!(cfg.style, GlowStyle::Laser) && !s.typing {
                0.0
            } else {
                tf
            };
            let mut cov_f = peak_f * tf;
            let mut full_f = cov_f;
            if matches!(cfg.style, GlowStyle::Fire) {
                cov_f = cov_f.min(Self::FIRE_STREAK_COV_CAP);
                full_f = cov_f;
                if fire_text
                    .as_ref()
                    .is_some_and(|text| text.over_text(i32::from(s.row), i32::from(s.col)))
                {
                    cov_f = 0.0;
                } else if cfg.dark_theme {
                    let color = self.comet_color(cfg, pos);
                    let key = (color, pos.to_bits());
                    let lifts = match fire_lift_memo {
                        Some((k, l)) if k == key => l,
                        _ => {
                            let l = fire_band_lifts(stack, color, pos, fire_core, chf);
                            fire_lift_memo = Some((key, l));
                            l
                        }
                    };
                    cov_f = self.ink_ledger.admit_axis((s.row, s.col), &lifts, cov_f);
                }
            }
            if matches!(cfg.style, GlowStyle::Laser) {
                let ripple = 0.86 + 0.14 * (s.pos * 9.0 - age * Self::LASER_RIPPLE_OMEGA).sin();
                cov_f *= ripple;
                full_f *= ripple;
            }
            let cov = cov_f as u8;
            let here = (s.row, s.col);
            if cov == 0 {
                prev = Some(here);
                continue;
            }
            let (x, y) = center(s.row, s.col);
            let sample = CometSample { x, y, cov, pos };
            let mut runs = CometRuns {
                runs: &mut self.comet_runs,
                run: &mut self.comet_run,
                typing: &mut self.comet_typing,
                run_typing: &mut self.comet_run_typing,
            };
            runs.chain(&mut runs_len, prev, here, sample, s.typing);
            prev = Some(here);
            head_full = full_f.min(255.0) as u8;
        }
        CometRuns {
            runs: &mut self.comet_runs,
            run: &mut self.comet_run,
            typing: &mut self.comet_typing,
            run_typing: &mut self.comet_run_typing,
        }
        .publish(&mut runs_len);
        if runs_len == 0 {
            return;
        }
        let mut bridge: Option<[CometSample; 2]> = None;
        let mut caret_sample = false;
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && let Some(last) = self.comet_runs.get_mut(runs_len - 1)
            && let Some(head) = last.last().copied()
        {
            let (_, y) = center(cr, cc);
            let x = oxf + (cc as f32 + cfg.head_dx) * cwf;
            if (x - head.x).abs() <= cwf * 1.5 && (y - head.y).abs() <= chf * 1.5 {
                let pos = if matches!(cfg.style, GlowStyle::Phaser) {
                    0.5
                } else {
                    1.0
                };
                let fire = matches!(cfg.style, GlowStyle::Fire);
                if bed.is_some() {
                    last.push(CometSample {
                        x,
                        y,
                        cov: head_full.max(head.cov),
                        pos,
                    });
                    caret_sample = true;
                } else if cfg.dark_theme {
                    let full = if fire {
                        (head_full as f32 * Self::FIRE_HEAD_COV_CAP / Self::FIRE_STREAK_COV_CAP)
                            .min(Self::FIRE_HEAD_COV_CAP) as u8
                    } else {
                        head_full
                    };
                    let (dx, dy) = (x - head.x, y - head.y);
                    let exit = |d: f32, half: f32| {
                        if d.abs() > 1e-3 {
                            half / d.abs()
                        } else {
                            f32::INFINITY
                        }
                    };
                    let t = exit(dx, cwf * 0.5).min(exit(dy, chf * 0.5));
                    if t < 1.0 {
                        let knee = CometSample {
                            x: head.x + dx * t,
                            y: head.y + dy * t,
                            cov: head.cov,
                            pos,
                        };
                        last.push(knee);
                        bridge = Some([
                            knee,
                            CometSample {
                                x,
                                y,
                                cov: full.max(head.cov),
                                pos,
                            },
                        ]);
                    } else {
                        last.push(CometSample {
                            x,
                            y,
                            cov: head.cov,
                            pos,
                        });
                    }
                } else {
                    last.push(CometSample {
                        x,
                        y,
                        cov: head.cov,
                        pos,
                    });
                }
            }
        }

        if let (Some(samples), Some(kinds)) = (
            self.comet_runs.get(runs_len - 1),
            self.comet_typing.get_mut(runs_len - 1),
        ) {
            let head = kinds.last().copied().unwrap_or(true);
            kinds.resize(samples.len(), head);
        }

        if !cfg.dark_theme {
            if self.water_seen.len() == Self::WATER_OWNER_SLOTS {
                self.water_seen.fill(0);
            } else {
                self.water_seen.clear();
                self.water_seen.resize(Self::WATER_OWNER_SLOTS, 0);
            }
            let (oxi, oyi) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
            for run in self.comet_runs[..runs_len].iter_mut().rev() {
                for s in run.iter_mut().rev() {
                    let (col, row) = (
                        ((s.x - oxi as f32) / cwf).floor() as i32,
                        ((s.y - oyi as f32) / chf).floor() as i32,
                    );
                    if col < 0 || row < 0 || col >= geom.cols as i32 || row >= geom.rows as i32 {
                        continue;
                    }
                    if !Self::claim_water_cell(&mut self.water_seen, row as u16, col as u16) {
                        s.cov = 0;
                    }
                }
            }
            #[cfg(test)]
            let heads = if self.heads_square { None } else { cur };
            #[cfg(not(test))]
            let heads = cur;
            self.emit_comet_light(now, cfg, geom, heads, runs_len, under);
            return;
        }

        let aura_head = bridge.map(|b| (b[1].x, b[1].y)).or_else(|| {
            self.comet_runs
                .get(runs_len - 1)
                .and_then(|r| r.last())
                .map(|s| (s.x, s.y))
        });
        let runs_len = if let Some(shape) = bed {
            #[cfg(test)]
            let heads = if self.heads_square { None } else { cur };
            #[cfg(not(test))]
            let heads = cur;
            self.emit_dark_bed(
                cfg,
                geom,
                runs_len,
                shape,
                &|g: &CursorGlow, pos| g.comet_color(cfg, pos),
                heads,
                under,
            );
            #[cfg(not(test))]
            let split = false;
            #[cfg(test)]
            let split = self.leap_split;
            self.split_air_runs(
                geom,
                runs_len,
                caret_sample,
                !split && matches!(cfg.style, GlowStyle::Beam),
            )
        } else {
            runs_len
        };
        let core_thick = match cfg.style {
            GlowStyle::Phaser => (chf * 0.66).max(8.0),
            GlowStyle::Fire => fire_core,
            GlowStyle::Laser => (chf * 0.26).max(3.5),
            GlowStyle::Beam => (chf * (0.08 + 0.26 * beam_pw)).max(2.0),
            GlowStyle::Sparkle => (chf * 0.45).max(5.0),
            _ => (chf * 0.13).max(2.0),
        };
        let straighten = cwf.max(chf) * 0.8;

        let mut beam_scratch = std::mem::take(&mut self.beam_scratch);
        let aura_from = halos.len();
        let color_at = |pos: f32| self.comet_color(cfg, pos);
        let runs = if bed.is_some() {
            &self.air_runs[..runs_len]
        } else {
            &self.comet_runs[..runs_len]
        };
        for (i, r) in runs.iter().enumerate() {
            #[cfg(not(test))]
            let chords_rows = false;
            #[cfg(test)]
            let chords_rows = self.streak_chords_rows;
            let one_row = !chords_rows && r.iter().all(|s| s.y == r[0].y);
            let tol = if one_row || matches!(cfg.style, GlowStyle::Phaser) {
                0.0
            } else {
                straighten
            };
            let bridge = bridge.filter(|_| i + 1 == runs_len);
            let stub = (bed.is_some() && caret_sample && i + 1 == runs_len && Self::is_bridge(r))
                .then(|| cur.map(|(cr, cc)| Self::caret_box(geom, cr, cc)))
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
            layered_beam_quads_with(
                &mut beam_scratch,
                out,
                geom.beam_clip(),
                r,
                core_thick,
                tol,
                &color_at,
                stack,
            );
            if let Some(cell) = stub {
                clip_quads_to(out, from, cell);
            }
            if let Some(pair) = bridge.as_ref() {
                layered_beam_quads_with(
                    &mut beam_scratch,
                    out,
                    geom.beam_clip(),
                    pair,
                    core_thick,
                    0.0,
                    &color_at,
                    stack,
                );
            }
            if matches!(cfg.style, GlowStyle::Laser) {
                #[cfg(not(test))]
                let held = true;
                #[cfg(test)]
                let held = !self.laser_aura_unheld;
                let samples = &r[..r.len() - usize::from(bridge.is_some())];
                for s in samples.iter().chain(bridge.as_ref().map(|p| &p[1])) {
                    let reach = match aura_head {
                        Some((hx, hy)) if held => {
                            let d = ((s.x - hx) / cwf).hypot((s.y - hy) / chf);
                            1.0 - smoothstep01(
                                (d - Self::LASER_AURA_FULL_CELLS)
                                    / (Self::LASER_AURA_REACH_CELLS - Self::LASER_AURA_FULL_CELLS),
                            )
                        }
                        _ => 1.0,
                    };
                    let peak = (s.cov as f32 * Self::LASER_AURA_COV * reach).min(150.0) as u8;
                    if peak == 0 {
                        continue;
                    }
                    let (rx, ry) = if held {
                        hold_glow_radii_in_box(geom, (s.x, s.y), (cwf * 1.35, chf * 1.05))
                    } else {
                        (cwf * 1.35, chf * 1.05)
                    };
                    push_halo(halos, geom, s.x, s.y, rx, ry, premul_rgb(cfg.color, peak));
                }
            }
        }
        self.beam_scratch = beam_scratch;
        self.ink_ledger
            .admit_halos_masked(halos, aura_from, InkLedger::HEAD_FLOOR);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "clock, config, geometry, the caret, the runs and the stream"
    )]
    fn emit_comet_light(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        runs_len: usize,
        under: &mut Vec<GlowQuad>,
    ) {
        use aterm_render::{GlowBlend, RibbonVertex, ribbon_beam};
        if !matches!(
            cfg.style,
            GlowStyle::Comet
                | GlowStyle::Beam
                | GlowStyle::Laser
                | GlowStyle::Phaser
                | GlowStyle::Sparkle
                | GlowStyle::Lumen
        ) {
            return;
        }
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let body_cap = LIGHT_BODY_ALPHA_CAP;
        let beam_pw = if matches!(cfg.style, GlowStyle::Beam) {
            self.sparks.last().map_or(1.0, |s| {
                let age = now.saturating_duration_since(s.born).as_secs_f32();
                beam_power((age / s.life).clamp(0.0, 1.0))
            })
        } else {
            1.0
        };
        let ground = self.light_ground(cfg);
        let ink_memo = std::cell::Cell::new(None::<(u32, u32)>);
        let ink = |rgb: u32| match ink_memo.get() {
            Some((prev, solved)) if prev == rgb => solved,
            _ => {
                let solved = ground.body_ink(rgb);
                ink_memo.set(Some((rgb, solved)));
                solved
            }
        };
        let clip = geom.beam_clip();
        let stride = (geom.cw / 8).max(1);
        let budget = under.len().saturating_add(Self::MAX_QUADS);
        let mut verts = std::mem::take(&mut self.light_verts);
        let pass = |under: &mut Vec<GlowQuad>,
                    verts: &mut Vec<RibbonVertex>,
                    run: &[CometSample],
                    feather: bool,
                    layer: &dyn Fn(&CometSample) -> (f32, f32, f32, u32)| {
            verts.clear();
            let mut push = |x: f32, s: &CometSample| {
                let (dy, reach, alpha, color) = layer(s);
                let available = (chf * 0.5 - dy.abs()).max(0.0);
                let reach = reach.clamp(0.5_f32.min(available), available);
                verts.push(RibbonVertex {
                    x,
                    spine: s.y + dy,
                    up: reach,
                    dn: reach,
                    core_up: reach * 0.5,
                    core_dn: reach * 0.5,
                    color,
                    cov: alpha.clamp(0.0, body_cap),
                    lift: 0.0,
                    lift_span: 0.0,
                });
            };
            if let [lone] = run {
                push(lone.x - cwf * 0.45, lone);
                push(lone.x + cwf * 0.45, lone);
            } else {
                if feather && let [first, second, ..] = run {
                    let dir = if second.x < first.x { -1.0 } else { 1.0 };
                    push(first.x - dir * cwf * 0.5, &CometSample { cov: 0, ..*first });
                }
                for s in run {
                    push(s.x, s);
                }
            }
            ribbon_beam(under, clip, verts, 1.0, stride, budget, GlowBlend::Over);
        };
        let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
        let row_of = |s: &CometSample| ((s.y - oyf) / chf).floor() as i32;
        let cell_of = |s: &CometSample| (row_of(s), ((s.x - oxf) / cwf).floor() as i32);
        let probe = self
            .row_cur_meta
            .as_ref()
            .map(|m| (m.row, &self.row_cur))
            .or_else(|| self.row_prev_meta.as_ref().map(|m| (m.row, &self.row_prev)));
        let wide = |s: &CometSample| {
            let (row, col) = cell_of(s);
            probe.is_some_and(|(probed, chars)| {
                i32::from(probed) == row
                    && usize::try_from(col).is_ok_and(|col| {
                        chars.get(col) == Some(&'\0') || chars.get(col + 1) == Some(&'\0')
                    })
            })
        };
        let after_wide = |s: &CometSample| {
            let (row, col) = cell_of(s);
            probe.is_some_and(|(probed, chars)| {
                i32::from(probed) == row
                    && usize::try_from(col - 1).is_ok_and(|prev| chars.get(prev) == Some(&'\0'))
            })
        };
        let (fg, bg) = (cfg.theme_fg, cfg.theme_bg);
        let caret = cur.map(|(r, c)| (i32::from(r), i32::from(c)));
        let mut band_run = std::mem::take(&mut self.light_band_run);
        let banded = |under: &mut Vec<GlowQuad>,
                      verts: &mut Vec<RibbonVertex>,
                      band_run: &mut Vec<CometSample>,
                      run: &[CometSample],
                      band: &dyn Fn(&CometSample) -> (f32, f32, f32, u32)| {
            let tail = match run {
                [first, second, ..] => {
                    let dir = if second.x < first.x { -1.0 } else { 1.0 };
                    Some((first.x - dir * cwf * 0.5, dir))
                }
                _ => None,
            };
            let run = match (tail, run.last()) {
                (Some((_, dir)), Some(&head)) if caret.is_some_and(|c| c != cell_of(&head)) => {
                    let from = head.x + dir * cwf * (0.5 - bed::DARK_BED_HEAD_FEATHER_CELLS);
                    band_run.clear();
                    band_run.extend(run.iter().copied().take_while(|s| (s.x - from) * dir < 0.0));
                    let knots = Self::LIGHT_BAND_HEAD_KNOTS;
                    band_run.extend((0..=knots).map(|j| {
                        let u = j as f32 / knots as f32;
                        let k = u * u * (3.0 - 2.0 * u);
                        CometSample {
                            x: from + dir * cwf * bed::DARK_BED_HEAD_FEATHER_CELLS * u,
                            cov: (f32::from(head.cov) * (1.0 - k)).round() as u8,
                            ..head
                        }
                    }));
                    &band_run[..]
                }
                _ => run,
            };
            pass(under, verts, run, true, &|s| {
                let (dy, reach, alpha, color) = band(s);
                let alpha = tail.map_or(alpha, |(x0, dir)| {
                    let k =
                        ((s.x - x0) * dir / (cwf * Self::LIGHT_BAND_TAIL_CELLS)).clamp(0.0, 1.0);
                    alpha * k * k * (3.0 - 2.0 * k)
                });
                if wide(s) || after_wide(s) {
                    let held = light_alpha_keeping(color, fg, bg, LIGHT_WIDE_BED_KEEP);
                    (dy, reach, alpha.min(held), color)
                } else {
                    (dy, reach, alpha, color)
                }
            });
        };
        for run in &self.comet_runs[..runs_len] {
            let Some(head) = run.last() else {
                continue;
            };
            let head_row = row_of(head);
            let last_off_row = run.iter().rposition(|s| row_of(s) != head_row);
            let run = match (cfg.style, last_off_row) {
                (GlowStyle::Laser, _) | (_, None) => &run[..],
                (_, Some(last_off)) => &run[last_off + 1..],
            };
            match cfg.style {
                GlowStyle::Comet => {
                    let color = ink(cfg.color);
                    let head = run[run.len() - 1];
                    let from = run.len().checked_sub(2).map(|i| run[i]);
                    let back = match from {
                        Some(prev) if prev.x > head.x => 1.0,
                        _ => -1.0,
                    };
                    const KNOTS: usize = 14;
                    const CAP_KNOTS: usize = 5;
                    let tail = cwf * 3.5;
                    let ox = f32::from(geom.origin_x);
                    let col = ((head.x - ox) / cwf).floor();
                    let (lead_edge, trail_edge) = if back < 0.0 {
                        (ox + (col + 1.0) * cwf, ox + col * cwf)
                    } else {
                        (ox + col * cwf, ox + (col + 1.0) * cwf)
                    };
                    let cap = (lead_edge - head.x).abs().min(cwf * 0.65);
                    let mut wedge = [head; KNOTS + CAP_KNOTS + 1];
                    for (k, knot) in wedge.iter_mut().enumerate() {
                        if k <= KNOTS {
                            let toward_head = k as f32 / KNOTS as f32;
                            knot.x = head.x + back * tail * (1.0 - toward_head);
                            knot.pos = toward_head;
                        } else {
                            let ahead = (k - KNOTS) as f32 / CAP_KNOTS as f32;
                            knot.x = head.x - back * cap * ahead;
                            knot.pos = 1.0 + ahead;
                        }
                    }
                    let q0 = under.len();
                    pass(under, &mut verts, &wedge, false, &|s| {
                        let (round, along) = if s.pos > 1.0 {
                            let a = s.pos - 1.0;
                            ((1.0 - a * a).max(0.0).sqrt(), 1.0)
                        } else {
                            (1.0, s.pos)
                        };
                        (
                            chf * 0.04,
                            chf * (0.06 + 0.40 * along) * round,
                            (f32::from(s.cov) * 4.0).min(body_cap) * along.sqrt() * round * round,
                            color,
                        )
                    });
                    let lead = lead_edge.round() as i32;
                    let oy = i32::from(geom.origin_y);
                    let row_top = oy
                        + ((head.y - f32::from(geom.origin_y)) / chf).floor() as i32
                            * geom.ch as i32;
                    let hold = if back < 0.0 {
                        (i32::MIN, row_top, lead, row_top + geom.ch as i32)
                    } else {
                        (lead, row_top, i32::MAX, row_top + geom.ch as i32)
                    };
                    clip_quads_to(under, q0, hold);
                    let fresh = (f32::from(head.cov) / 255.0 * 4.0).min(1.0);
                    let t = (f32::from(head.cov) * 4.0).min(LIGHT_BODY_ALPHA_CAP);
                    let remainder = if t >= 255.0 {
                        0.0
                    } else {
                        ((LIGHT_BODY_ALPHA_CAP - t) / (1.0 - t / 255.0)).max(0.0)
                    };
                    let peak = remainder.min(LIGHT_BODY_ALPHA_CAP) * fresh;
                    if peak >= 1.0 {
                        let q1 = under.len();
                        let budget = under.len().saturating_add(Self::MAX_QUADS);
                        push_ink_ellipse(
                            under,
                            geom,
                            (trail_edge + back * cwf * 0.22, head.y + chf * 0.04),
                            (cwf * 0.62, cwf * 0.62),
                            chf * 0.36,
                            color,
                            peak,
                            budget,
                        );
                        clip_quads_to(under, q1, hold);
                    }
                    if fresh > 0.02 {
                        let budget = under.len().saturating_add(Self::MAX_QUADS);
                        push_ink_ring(
                            under,
                            geom,
                            (head.x, head.y),
                            chf * Self::COMET_LIGHT_COMA_R,
                            (chf * Self::COMET_LIGHT_COMA_RIM).max(1.5),
                            color,
                            (body_cap * 0.28 * fresh, body_cap * 0.92 * fresh),
                            budget,
                        );
                    }
                    let born = self.sparks.last().map_or(now, |s| s.born);
                    let age = now.saturating_duration_since(born).as_secs_f32();
                    let arm = chf * Self::COMET_LIGHT_GLINT_ARM;
                    let thin = (chf * 0.045).max(1.0);
                    let row_top = f32::from(geom.origin_y)
                        + ((head.y - f32::from(geom.origin_y)) / chf).floor() * chf;
                    let descends = |x: f32| {
                        let col = ((x - f32::from(geom.origin_x)) / cwf).floor() as i32;
                        probe.is_some_and(|(probed, chars)| {
                            i32::from(probed) == row_of(&head)
                                && usize::try_from(col).is_ok_and(|col| {
                                    chars.get(col).is_some_and(|c| {
                                        DESCENDER_GLYPHS.contains(*c) || !c.is_ascii()
                                    })
                                })
                        })
                    };
                    #[cfg(test)]
                    self.light_glints.clear();
                    for (k, &(behind, high)) in Self::COMET_LIGHT_GLINTS.iter().enumerate() {
                        let twinkle = twinkle_env(age, k as f32 * 2.1 + head.x * 0.013);
                        let peak = LIGHT_BODY_ALPHA_CAP * 0.85 * fresh * twinkle;
                        if peak < 1.0 {
                            continue;
                        }
                        let x = head.x + back * cwf * behind;
                        let low = !high && !descends(x);
                        let centre = (
                            x,
                            row_top
                                + chf
                                    * if low {
                                        Self::COMET_LIGHT_GLINT_LOW
                                    } else {
                                        Self::COMET_LIGHT_GLINT_HIGH
                                    },
                        );
                        let rise = chf
                            * if low {
                                Self::COMET_LIGHT_GLINT_RISE.1
                            } else {
                                Self::COMET_LIGHT_GLINT_RISE.0
                            };
                        if wide(&CometSample {
                            x: centre.0,
                            y: head.y,
                            ..head
                        }) {
                            continue;
                        }
                        let budget = under.len().saturating_add(Self::MAX_QUADS);
                        let q2 = under.len();
                        push_ink_ellipse(
                            under,
                            geom,
                            centre,
                            (arm, arm),
                            thin,
                            color,
                            peak,
                            budget,
                        );
                        push_ink_ellipse(
                            under,
                            geom,
                            centre,
                            (thin, thin),
                            rise,
                            color,
                            peak,
                            budget,
                        );
                        clip_quads_to(under, q2, hold);
                        #[cfg(test)]
                        self.light_glints.push(centre);
                    }
                }
                GlowStyle::Lumen => {
                    let color = ink(cfg.color);
                    pass(under, &mut verts, run, true, &|s| {
                        (
                            chf * 0.04,
                            chf * 0.16,
                            (f32::from(s.cov) * 1.2).min(body_cap * 0.85),
                            color,
                        )
                    });
                }
                GlowStyle::Beam => {
                    let body = ink(cfg.color);
                    banded(under, &mut verts, &mut band_run, run, &|s| {
                        (0.0, chf * 0.40, f32::from(s.cov) * 0.55 * beam_pw, body)
                    });
                }
                GlowStyle::Laser => {
                    let color = ink(cfg.color);
                    pass(under, &mut verts, run, false, &|s| {
                        (
                            chf * 0.08,
                            chf * 0.20,
                            (f32::from(s.cov) * 0.12).min(28.0),
                            color,
                        )
                    });
                    pass(under, &mut verts, run, false, &|s| {
                        (
                            chf * 0.08,
                            chf * 0.07,
                            (f32::from(s.cov) * 0.45).min(96.0),
                            color,
                        )
                    });
                }
                GlowStyle::Phaser => {
                    banded(under, &mut verts, &mut band_run, run, &|s| {
                        (
                            0.0,
                            chf * 0.40,
                            f32::from(s.cov) * 1.6,
                            ink(self.comet_color(cfg, s.pos)),
                        )
                    });
                }
                GlowStyle::Sparkle => {
                    #[cfg(not(test))]
                    let vivid = true;
                    #[cfg(test)]
                    let vivid = !self.sparkle_paper_bold_ink;
                    let vivid_memo = std::cell::Cell::new(None::<(u32, u32)>);
                    banded(under, &mut verts, &mut band_run, run, &|s| {
                        let color = if vivid {
                            let key = s.pos.to_bits();
                            match vivid_memo.get() {
                                Some((prev, solved)) if prev == key => solved,
                                _ => {
                                    let solved = ground
                                        .vivid_body_ink(Self::sparkle_paper_gold_at(cfg, s.pos));
                                    vivid_memo.set(Some((key, solved)));
                                    solved
                                }
                            }
                        } else {
                            ink(self.comet_color(cfg, s.pos))
                        };
                        (0.0, chf * 0.40, f32::from(s.cov) * 0.55, color)
                    });
                }
                _ => {}
            }
        }
        self.light_verts = verts;
        self.light_band_run = band_run;
    }

    /// Emit per-pixel fire patches for each hot cell of the envelope.
    #[allow(
        clippy::too_many_arguments,
        reason = "clock, config, geometry, the five streams the fire can land in and the over-ink account"
    )]
    pub(super) fn emit_flames(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        patches: &mut Vec<FirePatch>,
        _charred: &mut Vec<CharFg>,
        halo_cells: &mut Vec<FireHaloCell>,
        engulf: &mut Vec<(u16, u16, f32)>,
        under: &mut Vec<GlowQuad>,
        ledger: &mut InkLedger,
    ) {
        engulf.clear();
        if !matches!(cfg.style, GlowStyle::Fire) || self.sparks.is_empty() {
            return;
        }
        let ch = geom.ch as f32;
        let t = self.fire_t();
        let cov_cap = (22.0 + 12.0 * t).min(255.0);
        let mut cells: [(u16, u16, f32); 16] = [(0, 0, 0.0); 16];
        let mut typed_cell = [false; 16];
        let mut n_cells = 0usize;
        for s in self.sparks.iter().rev() {
            if (s.row as usize) >= geom.rows || (s.col as usize) >= geom.cols {
                continue;
            }
            let slot = cells[..n_cells]
                .iter()
                .position(|c| c.0 == s.row && c.1 == s.col);
            if slot.is_none() && n_cells >= cells.len() {
                continue;
            }
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            let u = (age / s.life).clamp(0.0, 1.0);
            let env = if s.typing && !cfg.dark_theme {
                if u < 0.30 {
                    1.0
                } else {
                    0.5 * (1.0 + (std::f32::consts::PI * (u - 0.30) / 0.70).cos())
                }
            } else if s.typing {
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
            let strength = if cfg.dark_theme {
                env * (0.16 + 0.84 * s.pos.powf(1.4))
            } else {
                env * (0.40 + 0.60 * s.pos)
            };
            if strength < 0.02 {
                continue;
            }
            match slot {
                Some(i) => {
                    cells[i].2 = cells[i].2.max(strength);
                    typed_cell[i] |= s.typing;
                }
                None => {
                    cells[n_cells] = (s.row, s.col, strength);
                    typed_cell[n_cells] = s.typing;
                    n_cells += 1;
                }
            }
        }
        if n_cells == 0 {
            return;
        }
        for (rank, c) in cells[..n_cells].iter_mut().enumerate().skip(12) {
            c.2 *= ((16 - rank.min(16)) as f32 / (16 - 11) as f32).min(1.0);
        }
        let cells = &cells[..n_cells];
        if !cfg.dark_theme {
            self.emit_flames_light(cfg, geom, cells, t, under);
            return;
        }
        let phase = (self.flame_phase * 1024.0) as u32;
        let lean_q = (-(ch * 0.30 * t) * 4.0).clamp(-127.0, 127.0) as i8;
        let (fx_l, fx_r) = (geom.fx_left(), geom.fx_right());
        let chi = geom.ch as i32;
        let cwi = geom.cw as i32;
        let head = i32::from(geom.head);
        let oy = geom.origin_y as i32;
        let ox = geom.origin_x as i32;
        let root_floor = (chi - head).max(2);
        let lean_px = (lean_q as i32).abs() / 4 + 1;
        let shift = if lean_q > 0 { lean_px } else { -lean_px };
        let sub = Self::FIRE_SUBCOLS;
        let text = self.text_view();
        let air_cap = Self::FIRE_DARK_AIR_CAP_COLD + Self::FIRE_DARK_AIR_CAP_HEAT * t;
        let shoulder = (ch * 0.35).max(1.0);
        let smooth = |k: f32| {
            let k = k.clamp(0.0, 1.0);
            k * k * (3.0 - 2.0 * k)
        };
        let mut seen_rows = [0u16; 16];
        let mut n_rows = 0usize;
        for &(row, _, _) in cells {
            if seen_rows[..n_rows].contains(&row) {
                continue;
            }
            seen_rows[n_rows] = row;
            n_rows += 1;
            let air = row > 0;
            let row_i = i32::from(row);
            let page = cfg.theme_bg & 0x00FF_FFFF;
            #[cfg(not(test))]
            let rise_free = false;
            #[cfg(test)]
            let rise_free = self.fire_rises_into_text;
            let inked_above =
                |col: i32| air && !rise_free && self.flame_cell_inked(&text, page, row_i - 1, col);
            let mut burning = [(0u16, 0u16, 0.0f32); 16];
            let mut n_burning = 0usize;
            for (c, &typed) in cells.iter().zip(&typed_cell).filter(|(c, _)| c.0 == row) {
                if !typed && inked_above(i32::from(c.1)) {
                    continue;
                }
                burning[n_burning] = *c;
                n_burning += 1;
            }
            if n_burning == 0 {
                continue;
            }
            if air {
                let (head_col, head_strength) = (i32::from(burning[0].1), burning[0].2);
                let back = if n_burning > 1 && i32::from(burning[1].1) > head_col {
                    1
                } else {
                    -1
                };
                let ahead = head_col - back;
                if ahead >= 0
                    && ahead < geom.cols as i32
                    && n_burning < burning.len()
                    && !burning[..n_burning].iter().any(|c| i32::from(c.1) == ahead)
                {
                    burning[n_burning] =
                        (row, ahead as u16, Self::FIRE_DARK_AIR_LEAD * head_strength);
                    n_burning += 1;
                }
                let typed_head = self
                    .sparks
                    .iter()
                    .rev()
                    .find(|s| s.row == row)
                    .is_some_and(|s| s.typing);
                let ranks = if typed_head {
                    &Self::FIRE_DARK_AIR_RANKS[..]
                } else {
                    &Self::FIRE_DARK_AIR_RANKS[..1]
                };
                for (rank, share) in ranks.iter().enumerate().skip(1) {
                    let col = head_col + back * rank as i32;
                    if col < 0 || col >= geom.cols as i32 || !text.covers_cell(row_i, col) {
                        break;
                    }
                    let floor = share * head_strength;
                    let col = col as u16;
                    match burning[..n_burning].iter().position(|c| c.1 == col) {
                        Some(i) => burning[i].2 = burning[i].2.max(floor),
                        None if n_burning < burning.len() => {
                            burning[n_burning] = (row, col, floor);
                            n_burning += 1;
                        }
                        None => {}
                    }
                }
            }
            let burning = &burning[..n_burning];
            let on_row = || burning.iter();
            let (cmin, cmax) = on_row().fold((i32::MAX, i32::MIN), |(lo, hi), c| {
                (lo.min(i32::from(c.1)), hi.max(i32::from(c.1)))
            });
            let air_temp = (t.max(Self::FIRE_DARK_AIR_TEMP) * 255.0) as u8;
            let gap_temp = ((Self::FIRE_DARK_AIR_TEMP
                + (1.0 - Self::FIRE_DARK_AIR_TEMP) * t.clamp(0.0, 1.0))
                * 255.0) as u8;
            let row_temp_floor = Self::FIRE_DARK_ROW_TEMP;
            let kept_temp =
                ((row_temp_floor + (1.0 - row_temp_floor) * t.clamp(0.0, 1.0)) * 255.0) as u8;
            let air_cap = if air {
                air_cap.max(Self::FIRE_DARK_AIR_TONGUE_CAP)
            } else {
                air_cap
            };
            let row_top = oy + (row as f32 * ch) as i32;
            let kept_base = oy + (row as f32 * ch + ch * Self::FIRE_DARK_ROW_ROOT) as i32;
            let air_base = if air {
                row_top
            } else {
                oy + ((row as f32 * ch + (ch * Self::FIRE_DARK_ROOT).max(2.0)) as i32)
                    .max(root_floor)
            };
            #[cfg(test)]
            let columns = self.fire_column_tongues;
            #[cfg(not(test))]
            let columns = false;
            let foot = (ch
                * if columns {
                    0.2
                } else {
                    Self::FIRE_DARK_AIR_FOOT
                })
            .max(4.0);
            let air_rise =
                ch * (0.16 + 5.80 * t * t).max(if air { Self::FIRE_DARK_AIR_PEAK } else { 0.0 });
            let kept_rise = {
                let cold = Self::FIRE_DARK_ROW_RISE_COLD;
                ((kept_base - row_top) as f32 - ch * Self::FIRE_DARK_ROW_HEADROOM).max(0.0)
                    * (cold + (1.0 - cold) * t)
            };
            let mut tongues = if air {
                self.dark_tongues(burning, (cmin, cmax), &inked_above)
            } else {
                DarkTongues::default()
            };
            // A row whose letters nobody named (no probe of it, no frame
            // cells) has no gap to stand a tongue in: it burns as one crest
            // held to its letters, as round 3 did.
            #[cfg(test)]
            let crest = self.fire_kept_crest || !self.flame_row_named(&text, row_i);
            #[cfg(not(test))]
            let crest = !self.flame_row_named(&text, row_i);
            // KEPT IN ITS ROW, THE FLAME RISES WHERE NO INK IS: a few
            // feathered tongues, each on the axis of a blank cell of the
            // burning run (a word's gap, the caret's cell and the one ahead
            // of it), rooted under the baseline, with its gold core, and none
            // on a letter, whose cell keeps only a tongue's held flank.
            // Round 3 stood the row's field as one crest along the whole
            // line under its letters, held to their bar: a muddy brown haze
            // in three bands with a hot bead line under it (2026-10-22,
            // `context`, all three judges).
            if air && !crest {
                let (head_col, head_strength) = (i32::from(burning[0].1), burning[0].2);
                let back = if burning.len() > 1 && i32::from(burning[1].1) > head_col {
                    1
                } else {
                    -1
                };
                let kept_any = tongues.as_slice().iter().any(|tg| tg.kept);
                let mut kept = DarkTongues::default();
                for &tg in tongues.as_slice() {
                    if !tg.kept {
                        kept.push(tg);
                    }
                }
                let hash01 = |i: i32, salt: u32| {
                    let h = aterm_render::fire_field::fire_hash(i as u32, salt, 0x7F4A_7C15);
                    (h >> 8) as f32 / (1u32 << 24) as f32
                };
                for col in (cmin - 1)..=(cmax + 1) {
                    let u = col as f32 + 0.5;
                    let ahead = (col - head_col) * -back;
                    let lead = if (1..=2).contains(&ahead) {
                        Self::FIRE_DARK_AIR_LEAD * head_strength
                    } else {
                        0.0
                    };
                    let heat = flame_heat(burning.iter(), u);
                    let e = heat.max(lead);
                    if !kept_any
                        || e < 0.03
                        || col < 0
                        || col >= geom.cols as i32
                        || !inked_above(col)
                        || !matches!(self.flame_cell_glyph(&text, row_i, col), FlameGlyph::Blank)
                    {
                        continue;
                    }
                    // Its heat eased up (a square root), and a gap inside
                    // the run held to a share of the head's, so a gap a few
                    // letters back still stands a tongue with its core, not
                    // an ember (the caret's lead burns its own share); its
                    // height flickers slowly as a tongue in the air does.
                    let inside = if (cmin..=cmax).contains(&col) {
                        Self::FIRE_DARK_GAP_FLOOR * head_strength.sqrt()
                    } else {
                        0.0
                    };
                    let e = heat.sqrt().max(lead).max(inside);
                    let rate =
                        Self::TONGUE_FLICKER_RATE + Self::TONGUE_FLICKER_SPREAD * hash01(col, 2);
                    let flick = 0.5
                        + 0.5
                            * (std::f32::consts::TAU * (self.flame_phase * rate + hash01(col, 3)))
                                .sin();
                    kept.push(DarkTongue {
                        u,
                        half: Self::FIRE_DARK_HALF_W * (0.75 + 0.25 * e),
                        size: e * Self::FIRE_DARK_GAP_TONGUE * (0.80 + 0.20 * flick),
                        e,
                        kept: true,
                    });
                }
                tongues = kept;
            }
            // Round 3's kept flame (a test's control): a blank cell between
            // the burning letters (a word's gap) carries a tongue of its own
            // on its axis, so its core burns there.
            if air && crest {
                for col in cmin..=cmax {
                    let u = col as f32 + 0.5;
                    let e = flame_heat(burning.iter(), u);
                    if e < 0.03
                        || !inked_above(col)
                        || !matches!(self.flame_cell_glyph(&text, row_i, col), FlameGlyph::Blank)
                        || tongues
                            .as_slice()
                            .iter()
                            .any(|tg| tg.kept && (tg.u - u).abs() < 0.3)
                    {
                        continue;
                    }
                    let e = e.sqrt();
                    tongues.push(DarkTongue {
                        u,
                        half: Self::FIRE_DARK_HALF_W * (0.75 + 0.25 * e),
                        size: e * Self::FIRE_DARK_GAP_TONGUE,
                        e,
                        kept: true,
                    });
                }
            }
            let tongues = tongues.as_slice();
            // The tallest tongue over column position `u` (cells): its rise
            // in px, how far inside it `u` stands (1 on its axis, 0 at its
            // side) and whether it keeps to its row. The grid's first row
            // burns its smoothed heat as one field instead.
            let low_top = row_top as f32 + ch * Self::FIRE_DARK_LOW_BAND;
            let column = |u: f32| -> (f32, f32, bool) {
                if !air {
                    return (air_rise * flame_heat(on_row(), u), 1.0, false);
                }
                let mut best = (0.0f32, 0.0f32, false);
                for tg in tongues {
                    // Kept in its row, a tongue is the field's own crest
                    // over the row's smoothed heat (one continuous field
                    // along the line, its pointed crests dying under the
                    // row's top), easing off at the run's ends over a
                    // footprint wide enough that neighbours overlap whole:
                    // an envelope per tongue stepped the silhouette at every
                    // sampling column there.
                    let reach = if tg.kept && crest {
                        Self::FIRE_DARK_KEPT_REACH
                    } else {
                        1.0
                    };
                    let d = ((u - tg.u) / (tg.half * reach)).abs();
                    if d >= 1.0 {
                        continue;
                    }
                    let h = if tg.kept && crest {
                        kept_rise * flame_heat(on_row(), u) * smooth((1.0 - d) * 2.0)
                    } else if tg.kept {
                        kept_rise
                            * Self::FIRE_DARK_GAP_RISE
                            * tg.size
                            * (1.0 - d.powf(Self::FIRE_DARK_TONGUE_SHAPE))
                    } else {
                        air_rise * tg.size * (1.0 - d.powf(Self::FIRE_DARK_TONGUE_SHAPE))
                    };
                    if h > best.0 {
                        best = (h, 1.0 - d, tg.kept);
                    }
                }
                best
            };
            let any_kept = tongues.iter().any(|tg| tg.kept);
            const LEVELS: usize = CursorGlow::FIRE_DARK_HELD_LEVELS;
            let mut held = [[0.0f32; LEVELS]; Self::FIRE_DARK_HELD_CELLS];
            let mut free_low = [false; Self::FIRE_DARK_HELD_CELLS];
            let (c_lo, span) = if any_kept {
                (
                    cmin - 2,
                    ((cmax - cmin + 5).max(0) as usize).min(Self::FIRE_DARK_HELD_CELLS),
                )
            } else {
                (0, 0)
            };
            let mut letter_memo = None;
            for i in 0..span {
                let col = c_lo + i as i32;
                if col < 0 || col >= geom.cols as i32 {
                    continue;
                }
                let letter = self.flame_dark_letter(cfg, &text, ledger, (row, col));
                let caps = match letter {
                    None => [air_cap; LEVELS],
                    Some((ink, ground, lift)) => {
                        let key = (ink, ground, lift.map(f32::to_bits));
                        match letter_memo {
                            Some((prev, cached)) if prev == key => cached,
                            _ => {
                                let solved = std::array::from_fn(|level| {
                                    flame_dark_letter_cap(
                                        flame_level_light(level),
                                        ink,
                                        ground,
                                        lift,
                                        Self::FIRE_DARK_ROW_BAR,
                                    )
                                    .min(air_cap)
                                });
                                letter_memo = Some((key, solved));
                                solved
                            }
                        }
                    }
                };
                held[i] = caps;
                free_low[i] = self.flame_cell_glyph(&text, row_i, col).free_below();
            }
            let used_levels = std::cell::Cell::new(0u32);
            let ease = Self::FIRE_DARK_ROW_EASE;
            let held_at = |u: f32, low: bool, level: usize| -> f32 {
                let level = level.min(LEVELS - 1);
                let cells_held = &held[..span];
                if !low {
                    used_levels.set(used_levels.get() | (1 << level));
                }
                let mut cap = air_cap;
                for c in (u - ease).floor() as i32..=(u + ease).floor() as i32 {
                    let i = c - c_lo;
                    let h = if i >= 0 && (i as usize) < cells_held.len() {
                        if low && free_low[i as usize] {
                            air_cap
                        } else {
                            cells_held[i as usize][level]
                        }
                    } else {
                        0.0
                    };
                    let d = (c as f32 - u).max(u - (c + 1) as f32).max(0.0);
                    cap = cap.min(h + (air_cap - h) * smooth(d / ease));
                }
                cap
            };
            let row_foot = (ch * Self::FIRE_DARK_ROW_FOOT).max(4.0);
            let row_fade = (ch * Self::FIRE_DARK_ROW_TOP_FADE).max(1.0);
            for col in (cmin - 1).max(0)..=(cmax + 1).min(geom.cols as i32 - 1) {
                for k in 0..sub {
                    let u = col as f32 + (k as f32 + 0.5) / sub as f32;
                    let (rise_px, side, kept) = column(u);
                    if rise_px < 1.5 {
                        continue;
                    }
                    let base_y = if kept { kept_base } else { air_base };
                    #[cfg(not(test))]
                    let curl = Self::FIRE_DARK_FOOT_CURL;
                    #[cfg(test)]
                    let curl = if self.fire_flat_feet {
                        0.0
                    } else if columns {
                        0.25
                    } else {
                        Self::FIRE_DARK_FOOT_CURL
                    };
                    let curl = if kept { 0.0 } else { curl };
                    let base_y = base_y - (ch * curl * (1.0 - side)) as i32;
                    let scallop = if air { smooth(side / 0.5) } else { 1.0 };
                    // Rise ceiling: the grid top relaxed by the head band — at
                    // head 0 this is the grid top exactly (the historical clamp).
                    // Kept in its row, the field's crests stand to the
                    // row's top, where the ceiling fades them: cut at the
                    // envelope, every crest over it stood flat.
                    let top_y = if kept && crest {
                        row_top
                    } else if kept {
                        (base_y - rise_px as i32 - 2).max(row_top)
                    } else {
                        (base_y - rise_px as i32 - 2).max(oy - head)
                    };
                    if top_y >= base_y {
                        continue;
                    }
                    let left = ox + col * cwi + shift;
                    let x0 = (left + k * cwi / sub).max(fx_l);
                    let x1 = (left + (k + 1) * cwi / sub).min(fx_r);
                    if x1 <= x0 {
                        continue;
                    }
                    let strength = flame_heat(on_row(), u);
                    let flame_end = base_y.min(geom.fx_bot());
                    let (ca, cb) = ((x0 - ox).div_euclid(cwi), (x1 - 1 - ox).div_euclid(cwi));
                    let on_text =
                        |b: i32| b >= 0 && (text.over_text(b, ca) || text.over_text(b, cb));
                    let u_mid = ((x0 + x1) as f32 * 0.5 - ox as f32) / cwi as f32;
                    let peak_h = (rise_px
                        * if kept {
                            Self::FIRE_DARK_ROW_FIELD_REACH
                        } else if air {
                            Self::FIRE_DARK_AIR_FIELD_REACH
                        } else {
                            1.0
                        }) as u16;
                    let peak_h = peak_h.max(1);
                    // A tongue in a gap burns from the air's floor, as hot
                    // as one in the air at its coolest (it stands on a blank
                    // cell, its flank over a letter held to the letter's bar
                    // at its hottest light), and the fire's heat carries it
                    // the rest of the way, so a sustained blaze still reads
                    // hotter than a cold start under a line of text (held at
                    // the air's floor alone, both burned one colour).
                    let row_temp = if kept && crest {
                        kept_temp
                    } else if kept {
                        gap_temp
                    } else if air {
                        air_temp
                    } else {
                        (t * 255.0) as u8
                    };
                    let field = aterm_render::fire_field::FireFieldParams {
                        base_y,
                        peak_h: i32::from(peak_h),
                        phase,
                        temp: i32::from(row_temp),
                        strength: 255,
                        lean: i32::from(lean_q),
                        cov_cap: 255,
                        cell_h: chi,
                        top_fade_y: oy,
                    };
                    let ceiling_at = |band: i32, mid: f32| {
                        if kept {
                            let py = mid.floor() as i32;
                            let idx = (py - 1..=py + 1)
                                .map(|y| aterm_render::fire_field::fire_add_index_at(&field, y))
                                .max()
                                .unwrap_or(0);
                            // A tongue in a gap holds its flank over a
                            // letter against its hottest light at every
                            // height: held per heat level, the flank stood
                            // in bands as the level stepped.
                            let idx = if crest { idx } else { 1 << 16 };
                            let level = flame_level_of(idx);
                            let over = held_at(u_mid, false, level);
                            #[cfg(test)]
                            let hard = self.fire_kept_cap_hard;
                            #[cfg(not(test))]
                            let hard = false;
                            let cap_ease = if hard {
                                (0.5 * (base_y as f32 - low_top)).max(1.0)
                            } else {
                                Self::FIRE_DARK_CAP_EASE_PX
                            };
                            let low = if mid >= low_top {
                                let under = held_at(u_mid, true, level);
                                over + (under - over) * smooth((mid - low_top) / cap_ease)
                            } else {
                                over
                            };
                            return low
                                * smooth((base_y as f32 - mid) / row_foot)
                                * smooth((mid - row_top as f32) / row_fade);
                        }
                        let foot_in = if air {
                            smooth((base_y as f32 - mid) / foot)
                        } else {
                            1.0
                        };
                        if on_text(band) {
                            cov_cap * foot_in
                        } else {
                            let top = (oy + band * chi) as f32;
                            let mut k = 1.0f32;
                            if band + 1 < geom.rows as i32
                                && !(air && band + 1 == row_i)
                                && on_text(band + 1)
                            {
                                k = k.min((top + ch - mid) / shoulder);
                            }
                            if on_text(band - 1) {
                                k = k.min((mid - top) / shoulder);
                            }
                            (cov_cap + (air_cap - cov_cap) * smooth(k)) * foot_in
                        }
                    };
                    let x0_u = x0 as u16;
                    let w_u = (x1 - x0) as u16;
                    let base_y_u = base_y as u16;
                    let cell_h = geom.ch as u16;
                    let rise_denom = rise_px.max(1.0);
                    let col_u = col as u16;
                    let mut yy = top_y;
                    while yy < flame_end {
                        if patches.len() >= Self::MAX_QUADS {
                            return;
                        }
                        let band = (yy - oy).div_euclid(chi);
                        let band_end = (oy + (band + 1) * chi).min(flame_end);
                        let coarse = (yy + Self::FIRE_SLICE_PX).min(band_end);
                        let c_start = ceiling_at(band, yy as f32 + 0.5);
                        let steep = coarse > yy + 1
                            && (c_start - ceiling_at(band, coarse as f32 - 0.5)).abs() > 3.0;
                        let end = if steep { yy + 1 } else { coarse };
                        if band < geom.rows as i32 {
                            let mid = 0.5 * (yy + end) as f32;
                            let rise = ((base_y as f32 - mid) / rise_denom).clamp(0.0, 1.0);
                            let ceiling = if end == yy + 1 {
                                c_start
                            } else {
                                ceiling_at(band, mid)
                            };
                            patches.push(FirePatch {
                                row: band.max(0) as u16,
                                x: x0_u,
                                y: yy as u16,
                                w: w_u,
                                h: (end - yy) as u16,
                                base_y: base_y_u,
                                peak_h,
                                phase,
                                temp: row_temp,
                                strength: 255,
                                lean: lean_q,
                                cov_cap: (ceiling
                                    * if kept && crest {
                                        1.0
                                    } else {
                                        flame_tip_fade(rise)
                                    }
                                    * scallop) as u8,
                                cell_h,
                                mode: FireMode::Add,
                            });
                            // A tongue kept in its row rings no letter: its
                            // flank is held to the letters' bar, and a ring
                            // drew each letter dark-outlined in its glow.
                            if band >= 0 && !(kept && !crest) {
                                let frac = (end - yy) as f32 / ch;
                                let rise_frac = 1.0
                                    - ((base_y - end) as f32 / rise_denom).clamp(0.0, 1.0) * 0.55;
                                let wgt =
                                    strength * frac * rise_frac * (0.35 + 0.55 * t) / sub as f32;
                                let key = (band as u16, col_u);
                                match engulf.iter_mut().find(|e| (e.0, e.1) == key) {
                                    Some(e) => e.2 += wgt,
                                    None => engulf.push((key.0, key.1, wgt)),
                                }
                            }
                        }
                        yy = end;
                    }
                }
            }
            let used = used_levels.get();
            if used != 0 {
                let luma = crate::color_math::relative_luminance_table();
                for (i, caps) in held[..span].iter().enumerate() {
                    let col = c_lo + i as i32;
                    if col < 0 || col >= geom.cols as i32 || !(cmin - 1..=cmax + 1).contains(&col) {
                        continue;
                    }
                    let mut best = (0.0f32, 0u32);
                    for (level, &cap_val) in caps.iter().enumerate() {
                        if used & (1 << level) == 0 || cap_val >= air_cap {
                            continue;
                        }
                        let hot = flame_level_light(level);
                        let cov = cap_val as u32;
                        let lit = |sh: u32| (((hot >> sh) & 0xff) * cov + 127) / 255;
                        let lit = (lit(16) << 16) | (lit(8) << 8) | lit(0);
                        let l = crate::color_math::relative_luminance_with(luma, lit);
                        if l > best.0 || best.1 == 0 {
                            best = (l, lit);
                        }
                    }
                    if best.1 != 0 {
                        ledger.add_under((row_i, col), best.1);
                    }
                }
            }
            #[cfg(not(test))]
            let cores = air;
            #[cfg(test)]
            let cores = air && !self.fire_coreless;
            if cores {
                self.emit_flame_cores(
                    geom,
                    &text,
                    (row, tongues, crest),
                    (air_base, kept_base, air_rise, kept_rise),
                    (t, shift),
                    under,
                );
            }
        }
        let luma_table = crate::color_math::relative_luminance_table();
        for &(er, ec, e) in engulf.iter() {
            const HALO_TOE: f32 = 0.05;
            let s = ((e - HALO_TOE) / (1.5 - HALO_TOE)).clamp(0.0, 1.0);
            let s = s * s * (3.0 - 2.0 * s);
            let strength = (s * 255.0) as u8;
            if strength == 0 {
                continue;
            }
            match self.frame_text.pair(i32::from(er), i32::from(ec)) {
                Some((None, _)) => continue,
                Some((Some(ink), ground))
                    if crate::color_math::relative_luminance_with(luma_table, ink)
                        <= crate::color_math::relative_luminance_with(luma_table, ground) =>
                {
                    continue;
                }
                _ => {}
            }
            halo_cells.push(FireHaloCell {
                row: er,
                col: ec,
                strength,
            });
        }
        halo_cells.sort_unstable_by_key(|c| (c.row, c.col));
    }

    fn flame_dark_letter(
        &self,
        cfg: &GlowConfig,
        text: &TextView<'_>,
        ledger: &InkLedger,
        (row, col): (u16, i32),
    ) -> Option<(u32, u32, [f32; 3])> {
        let r = i32::from(row);
        if text.knows_row(r) && !text.covers_cell(r, col) {
            return None;
        }
        #[cfg(test)]
        if self.fire_row_unheld {
            return None;
        }
        let (fg, bg) = (cfg.theme_fg & 0x00FF_FFFF, cfg.theme_bg & 0x00FF_FFFF);
        let (ink, ground) = match self.frame_text.pair(r, col) {
            Some((Some(ink), ground)) => (ink, ground),
            Some((None, ground)) => (fg, ground),
            None => (fg, bg),
        };
        let lift = ledger.laid_peak((r, col)).unwrap_or([0.0; 3]);
        Some((ink, ground, lift))
    }

    /// Whether cell `(row, col)` holds ink a dark tongue may not rise into.
    pub(super) fn flame_cell_inked(
        &self,
        text: &TextView<'_>,
        page: u32,
        row: i32,
        col: i32,
    ) -> bool {
        if row < 0 {
            return false;
        }
        if !text.knows_row(row) || text.covers_cell(row, col) {
            return true;
        }
        self.frame_text
            .pair(row, col)
            .is_some_and(|(ink, ground)| ink.is_some() || !reads_as_page(ground, page))
    }

    /// Whether anybody named what row `row` holds: the frame (the probe or
    /// its own cells) or the host's probe of the typing row.
    pub(super) fn flame_row_named(&self, text: &TextView<'_>, row: i32) -> bool {
        text.knows_row(row)
            || [self.row_cur_meta.as_ref(), self.row_prev_meta.as_ref()]
                .into_iter()
                .flatten()
                .any(|m| i32::from(m.row) == row)
    }

    /// What stands in cell `(row, col)` of a burning row for the dark
    /// flame's root: a blank (off the line, a space, a glyph-less cell) or a
    /// letter, which descends under the baseline unless the host's probe of
    /// the row names a glyph that does not ([`DESCENDER_GLYPHS`]; a wide
    /// glyph fills its box). A cell nobody named is a descending letter.
    pub(super) fn flame_cell_glyph(&self, text: &TextView<'_>, row: i32, col: i32) -> FlameGlyph {
        if text.knows_row(row) && !text.covers_cell(row, col) {
            return FlameGlyph::Blank;
        }
        let probed = [
            (self.row_cur_meta.as_ref(), &self.row_cur),
            (self.row_prev_meta.as_ref(), &self.row_prev),
        ]
        .into_iter()
        .find_map(|(meta, chars)| meta.filter(|m| i32::from(m.row) == row).map(|_| chars));
        if let Some(chars) = probed
            && let Ok(c) = usize::try_from(col)
            && let Some(&glyph) = chars.get(c)
        {
            return match glyph {
                ' ' => FlameGlyph::Blank,
                g => FlameGlyph::Letter {
                    descends: g == '\0'
                        || chars.get(c + 1) == Some(&'\0')
                        || !g.is_ascii()
                        || DESCENDER_GLYPHS.contains(g),
                },
            };
        }
        match self.frame_text.pair(row, col) {
            Some((None, _)) => FlameGlyph::Blank,
            _ => FlameGlyph::Letter { descends: true },
        }
    }

    fn dark_tongues(
        &self,
        burning: &[(u16, u16, f32)],
        (cmin, cmax): (i32, i32),
        inked_above: &dyn Fn(i32) -> bool,
    ) -> DarkTongues {
        let spacing = Self::FIRE_DARK_SPACING;
        let phase = self.flame_phase;
        let hash01 = |i: i32, salt: u32| {
            let h = aterm_render::fire_field::fire_hash(i as u32, salt, 0x3C6E_F372);
            (h >> 8) as f32 / (1u32 << 24) as f32
        };
        let (u_lo, u_hi) = (cmin as f32 - 0.8, cmax as f32 + 1.8);
        let mut out = DarkTongues::default();
        for i in (u_lo / spacing).floor() as i32..=(u_hi / spacing).ceil() as i32 {
            if out.n == out.buf.len() {
                break;
            }
            let u = (i as f32 + 0.5 + (hash01(i, 1) - 0.5) * 0.5) * spacing;
            let e = flame_heat(burning.iter(), u);
            if e < 0.03 {
                continue;
            }
            let rate = Self::TONGUE_FLICKER_RATE + Self::TONGUE_FLICKER_SPREAD * hash01(i, 2);
            let flick = 0.5 + 0.5 * (std::f32::consts::TAU * (phase * rate + hash01(i, 3))).sin();
            let size = e * (0.70 + 0.30 * hash01(i, 4)) * (0.72 + 0.28 * flick);
            let half = Self::FIRE_DARK_HALF_W * (0.75 + 0.25 * e);
            let kept = ((u - half).floor() as i32..(u + half).ceil() as i32).any(inked_above);
            out.buf[out.n] = DarkTongue {
                u,
                half,
                size,
                e,
                kept,
            };
            out.n += 1;
        }
        out
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "geometry, the text, the row's tongues, their roots and rises, the heat and the stream"
    )]
    fn emit_flame_cores(
        &self,
        geom: Geom,
        text: &TextView<'_>,
        (row, tongues, crest): (u16, &[DarkTongue], bool),
        (air_base, kept_base, air_rise, kept_rise): (i32, i32, f32, f32),
        (t, shift): (f32, i32),
        under: &mut Vec<GlowQuad>,
    ) {
        const MAX_CORES: usize = 48;
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
        let row_i = i32::from(row);
        let low_top = oyf + (f32::from(row) + Self::FIRE_DARK_LOW_BAND) * chf;
        let smooth = |k: f32| {
            let k = k.clamp(0.0, 1.0);
            k * k * (3.0 - 2.0 * k)
        };
        #[cfg(test)]
        let hard = self.fire_kept_cap_hard;
        #[cfg(not(test))]
        let hard = false;
        // A kept core's ceiling at window `(x, y)`: whole over a blank cell
        // and under a letter's baseline when it has no descender, nothing
        // over a letter's strokes; eased across a third of a cell and a few
        // pixels up.
        let cap_cell = |x: f32, y: f32| -> f32 {
            let c = ((x - oxf) / cwf).floor() as i32;
            match self.flame_cell_glyph(text, row_i, c) {
                FlameGlyph::Blank => 1.0,
                FlameGlyph::Letter { descends: true } => 0.0,
                // Under a letter with no descender, eased in over a few
                // pixels from the band's top: a core whole a pixel under it
                // stood a hot bead line along the baseline.
                FlameGlyph::Letter { descends: false } => {
                    if hard {
                        f32::from(u8::from(y >= low_top))
                    } else {
                        smooth((y - low_top) / Self::FIRE_DARK_CAP_EASE_PX)
                    }
                }
            }
        };
        let ease_x = cwf * 0.20;
        let cap_at = |x: f32, y: f32| {
            cap_cell(x - ease_x, y)
                .min(cap_cell(x + ease_x, y))
                .min(cap_cell(x, y - Self::FIRE_DARK_CORE_CLEAR_PX))
        };
        let lean = -cwf * 0.30 * t;
        let (fx_l, fx_r) = (geom.fx_left() as f32, geom.fx_right() as f32);
        let knot = Self::FIRE_LIGHT_KNOT_PX;
        let slab = Self::FIRE_LIGHT_SLAB_PX;
        #[cfg(test)]
        let columns = self.fire_column_tongues;
        #[cfg(not(test))]
        let columns = false;
        for kept in [false, true] {
            let mut cores = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); MAX_CORES];
            let mut n = 0usize;
            let base = if kept { kept_base } else { air_base } as f32;
            // A core in the air feathers in over its tongue's own foot: over
            // five pixels its gold heart stood on the floor the tongue's
            // feathered root no longer draws.
            let root_px = if kept {
                Self::FIRE_DARK_KEPT_CORE_ROOT_PX
            } else if columns {
                Self::FIRE_DARK_CORE_ROOT_PX
            } else {
                (chf * Self::FIRE_DARK_AIR_FOOT).max(Self::FIRE_DARK_CORE_ROOT_PX)
            };
            #[cfg(test)]
            let foot = if kept && !self.fire_kept_core_from_base {
                root_px
            } else {
                0.0
            };
            #[cfg(not(test))]
            let foot = if kept { root_px } else { 0.0 };
            let taper = if kept || columns {
                0.7
            } else {
                Self::FIRE_DARK_AIR_CORE_TAPER
            };
            for tg in tongues.iter().filter(|tg| tg.kept == kept) {
                if n == MAX_CORES {
                    break;
                }
                let rise = if kept { kept_rise } else { air_rise };
                let (reach, wide) = if kept || columns {
                    (Self::FIRE_DARK_CORE_REACH, Self::FIRE_DARK_CORE_W)
                } else {
                    (Self::FIRE_DARK_AIR_CORE_REACH, Self::FIRE_DARK_AIR_CORE_W)
                };
                let h = rise * tg.size * reach;
                if h < 3.0 {
                    continue;
                }
                // A core in the air stands in its tongue's body, which the
                // wind's lean set over by `shift` from root to tip: on the
                // tongue's own axis its gold stood beside the flame, a bare
                // needle at full heat (2026-10-23, `burst`).
                let over = if kept || columns { 0.0 } else { shift as f32 };
                cores[n] = (
                    oxf + tg.u * cwf + over,
                    tg.half * cwf * wide,
                    h,
                    if kept {
                        Self::FIRE_DARK_KEPT_CORE_COV
                    } else if columns {
                        Self::FIRE_DARK_CORE_COV
                    } else {
                        Self::FIRE_DARK_AIR_CORE_COV
                    } * tg.e.min(1.0)
                        * (0.75 + 0.25 * t),
                );
                n += 1;
            }
            let cores = &cores[..n];
            if cores.is_empty() {
                continue;
            }
            let peak_h = cores.iter().map(|c| c.2).fold(0.0f32, f32::max);
            let x_lo = cores
                .iter()
                .map(|c| c.0 - c.1 + lean.min(0.0))
                .fold(f32::INFINITY, f32::min)
                .max(fx_l);
            let x_hi = cores
                .iter()
                .map(|c| c.0 + c.1 + lean.max(0.0))
                .fold(f32::NEG_INFINITY, f32::max)
                .min(fx_r);
            if x_hi <= x_lo {
                continue;
            }
            // A gap's cores are sampled a pixel apart: ramped across two,
            // a core's steep side mixed its gold heart's light with its
            // cooled edge's and thinned the gold to olive there, with no
            // crest's body round it to warm it.
            let knot = if kept && !crest { 1.0 } else { knot };
            let knots = (((x_hi - x_lo) / knot).ceil() as usize).max(1);
            // Thin light burns amber at the warmest: a dim yellow over the
            // dark reads olive, a dim amber an ember. A core in a gap stands
            // over its own tongue alone, as a tongue in the air does, so it
            // thins to amber as the air's does.
            #[cfg(test)]
            let yellow_cov = if kept && crest && !self.fire_kept_core_from_base {
                Self::FIRE_DARK_KEPT_CORE_YELLOW_COV
            } else {
                Self::FIRE_DARK_CORE_YELLOW_COV
            };
            #[cfg(not(test))]
            let yellow_cov = if kept && crest {
                Self::FIRE_DARK_KEPT_CORE_YELLOW_COV
            } else {
                Self::FIRE_DARK_CORE_YELLOW_COV
            };
            let mut slab_cores = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); MAX_CORES];
            let mut y = (base - foot - peak_h).floor().max(geom.fx_top() as f32);
            while y < base {
                if under.len() >= Self::MAX_QUADS {
                    return;
                }
                // Kept in its row, a core's root and the band under the
                // letters' baseline it eases into are laid a pixel a slab:
                // in slabs of two its root ended in one step of most of its
                // light, the bottom edge of a hot line along the line.
                let fine = kept && !hard && y >= low_top - 1.0;
                let h = if fine { 1.0 } else { slab }.min(base - y);
                let ym = y + 0.5 * h;
                if ym >= base {
                    break;
                }
                let root_in = smooth((base - ym) / root_px);
                // Its light feathers in over the whole root, its colour
                // cools over the root's first few pixels alone: cooled over
                // the feathered root, the gold heart burned orange.
                let hue_in = smooth((base - ym) / root_px.min(Self::FIRE_DARK_CORE_ROOT_PX));
                let mut n_slab = 0usize;
                for &(xc, half, th, peak) in cores {
                    let rise = (base - foot - ym) / th;
                    if rise >= 1.0 {
                        continue;
                    }
                    let rise = rise.max(0.0);
                    let xc = xc + lean * rise * rise;
                    let k = (rise / 0.2).clamp(0.0, 1.0);
                    let hw =
                        half * (0.62 + 0.38 * k * k * (3.0 - 2.0 * k)) * (1.0 - rise).powf(taper);
                    if hw < 0.3 {
                        continue;
                    }
                    let peak_ym = peak * root_in * (1.0 - rise).powf(0.8);
                    #[cfg(test)]
                    let rise_base = if self.fire_core_uncooled {
                        rise
                    } else {
                        rise.max(1.0 - hue_in)
                    };
                    #[cfg(not(test))]
                    let rise_base = rise.max(1.0 - hue_in);
                    slab_cores[n_slab] = (xc, hw, peak_ym, rise_base);
                    n_slab += 1;
                }
                let active_cores = &slab_cores[..n_slab];
                if active_cores.is_empty() {
                    y += h;
                    continue;
                }
                let at = |x: f32| -> (u32, u32) {
                    let mut best = (0.0f32, 0.0f32);
                    for &(xc, hw, peak_ym, rise_base) in active_cores {
                        let d = ((x - xc) / hw).abs();
                        if d >= 1.0 {
                            continue;
                        }
                        let cov = peak_ym * (1.0 - d * d).powf(1.6);
                        if cov > best.0 {
                            #[cfg(test)]
                            let rise = if self.fire_core_uncooled {
                                rise_base
                            } else {
                                rise_base.max(d * d)
                            };
                            #[cfg(not(test))]
                            let rise = rise_base.max(d * d);
                            best = (cov, rise);
                        }
                    }
                    let cov = if kept && best.0 > 0.0 {
                        best.0 * cap_at(x, ym)
                    } else {
                        best.0
                    };
                    let cov = cov.min(255.0);
                    let thin = Self::FIRE_DARK_CORE_AMBER * smooth((yellow_cov - cov) / 38.0);
                    #[cfg(test)]
                    let thin = if self.fire_core_uncooled { 0.0 } else { thin };
                    (cov as u32, flame_core_ramp(best.1.max(thin)))
                };
                let mut prev = (x_lo, at(x_lo));
                for i in 1..=knots {
                    let x = (x_lo + i as f32 * knot).min(x_hi);
                    let here = at(x);
                    let (x0, (a0, c0)) = prev;
                    let (a1, c1) = here;
                    if a0 > 0 || a1 > 0 {
                        let (xi0, xi1) = (x0.round() as i32, x.round() as i32);
                        let (c0, c1) = (if a0 > 0 { c0 } else { c1 }, if a1 > 0 { c1 } else { c0 });
                        crate::effect_util::push_fx_ramp(
                            under,
                            geom,
                            xi0,
                            y as i32,
                            xi1 - xi0,
                            (y + h) as i32 - y as i32,
                            (premul_rgb(c0, a0 as u8), 0),
                            (premul_rgb(c1, a1 as u8), 0),
                        );
                    }
                    prev = (x, here);
                }
                y += h;
            }
        }
    }

    fn emit_flames_light(
        &self,
        cfg: &GlowConfig,
        geom: Geom,
        cells: &[(u16, u16, f32)],
        t: f32,
        under: &mut Vec<GlowQuad>,
    ) {
        const MAX_TONGUES: usize = 64;
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
        let text = self.text_view();
        let paints = flame_light_paint_bounds();
        let tall = chf * (Self::FIRE_LIGHT_RISE_COLD + Self::FIRE_LIGHT_RISE_HOT * t);
        let spacing = Self::FIRE_LIGHT_SPACING;
        let lean = -cwf * 0.35 * t;
        let phase = self.flame_phase;
        let hash01 = |i: i32, salt: u32| {
            let h = aterm_render::fire_field::fire_hash(i as u32, salt, 0x51F0_A3B7);
            (h >> 8) as f32 / (1u32 << 24) as f32
        };
        let mut seen_rows = [0u16; 16];
        let mut n_rows = 0usize;
        let mut row_cells = [(0u16, 0u16, 0.0f32); 16];
        for &(row, _, _) in cells {
            if seen_rows[..n_rows].contains(&row) {
                continue;
            }
            seen_rows[n_rows] = row;
            n_rows += 1;
            let mut n_row_cells = 0usize;
            let mut cmin = i32::MAX;
            let mut cmax = i32::MIN;
            for &c in cells {
                if c.0 == row && n_row_cells < row_cells.len() {
                    cmin = cmin.min(i32::from(c.1));
                    cmax = cmax.max(i32::from(c.1));
                    row_cells[n_row_cells] = c;
                    n_row_cells += 1;
                }
            }
            let row_slice = &row_cells[..n_row_cells];
            let envelope = |u: f32| flame_heat(row_slice.iter(), u);
            let root = oyf + (f32::from(row) + Self::FIRE_LIGHT_ROOT) * chf;
            let (u_lo, u_hi) = (cmin as f32 - 0.8, cmax as f32 + 1.8);
            let mut tongues = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); MAX_TONGUES];
            let mut n = 0usize;
            let first = (u_lo / spacing).floor() as i32;
            let last = (u_hi / spacing).ceil() as i32;
            for i in first..=last {
                if n == MAX_TONGUES {
                    break;
                }
                let u = (i as f32 + 0.5 + (hash01(i, 1) - 0.5) * 0.5) * spacing;
                let e = envelope(u);
                if e < 0.03 {
                    continue;
                }
                let rate = Self::TONGUE_FLICKER_RATE + Self::TONGUE_FLICKER_SPREAD * hash01(i, 2);
                let flick =
                    0.5 + 0.5 * (std::f32::consts::TAU * (phase * rate + hash01(i, 3))).sin();
                let h = tall * e * (0.70 + 0.30 * hash01(i, 4)) * (0.72 + 0.28 * flick);
                let half = cwf * Self::FIRE_LIGHT_HALF_W * (0.75 + 0.25 * e);
                let h = h.min(Self::flame_light_headroom(
                    &text,
                    row,
                    (u, half / cwf),
                    root,
                    (chf, oyf),
                ));
                if h < 2.0 {
                    continue;
                }
                tongues[n] = (oxf + u * cwf, h, half, e);
                n += 1;
            }
            let tongues = &tongues[..n];
            if tongues.is_empty() {
                continue;
            }
            let peak_h = tongues.iter().map(|tg| tg.1).fold(0.0f32, f32::max);
            let rows_up = ((peak_h - (root - (oyf + f32::from(row) * chf))) / chf)
                .ceil()
                .max(0.0) as i32;
            let (c_lo, c_hi) = (u_lo.floor() as i32 - 1, u_hi.ceil() as i32 + 1);
            let unknown_cap = std::cell::Cell::new(None::<f32>);
            let pair_memo = std::cell::Cell::new(None::<((u32, u32), f32)>);
            let caps = |r: i32, c: i32| -> f32 {
                if r < 0 || c < 0 || r >= geom.rows as i32 || c >= geom.cols as i32 {
                    return Self::FIRE_LIGHT_ALPHA;
                }
                self.flame_light_cap(cfg, &text, r, c, paints, &unknown_cap, &pair_memo)
            };
            let mut grid = [[0.0f32; 40]; 4];
            let span = ((c_hi - c_lo + 1) as usize).min(40);
            for (dr, line) in grid
                .iter_mut()
                .enumerate()
                .take((rows_up as usize + 1).min(4))
            {
                for (dc, slot) in line.iter_mut().enumerate().take(span) {
                    *slot = caps(i32::from(row) - dr as i32, c_lo + dc as i32);
                }
            }
            let cap_cell = |x: f32, y: f32| {
                let r = ((y - oyf) / chf).floor() as i32;
                let c = ((x - oxf) / cwf).floor() as i32;
                let dr = i32::from(row) - r;
                let dc = c - c_lo;
                if (0..4).contains(&dr) && dc >= 0 && (dc as usize) < span {
                    grid[dr as usize][dc as usize]
                } else {
                    caps(r, c)
                }
            };
            let ease_x = cwf * 0.34;
            let cap_at = |x: f32, y: f32| {
                cap_cell(x - ease_x, y)
                    .min(cap_cell(x + ease_x, y))
                    .min(cap_cell(x, y - 3.0))
                    .min(cap_cell(x, y + 3.0))
            };
            let x_lo = (oxf + u_lo * cwf).max(geom.fx_left() as f32);
            let x_hi = (oxf + u_hi * cwf).min(geom.fx_right() as f32);
            let knot = Self::FIRE_LIGHT_KNOT_PX;
            let knots = (((x_hi - x_lo) / knot).ceil() as usize).max(1);
            let slab = Self::FIRE_LIGHT_SLAB_PX;
            let mut slab_tongues = [(0.0f32, 0.0f32, 0.0f32, 0.0f32, 0u32); MAX_TONGUES];
            let mut y = (root - peak_h).floor().max(geom.fx_top() as f32);
            while y < root {
                if under.len() >= Self::MAX_QUADS {
                    return;
                }
                let h = slab.min(root - y);
                let ym = y + 0.5 * h;
                let mut n_slab = 0usize;
                for &(xc, th, half, _) in tongues {
                    let rise = (root - ym) / th;
                    if !(0.0..1.0).contains(&rise) {
                        continue;
                    }
                    let xc = xc + lean * rise * rise;
                    let base = {
                        let k = (rise / 0.2).clamp(0.0, 1.0);
                        0.62 + 0.38 * k * k * (3.0 - 2.0 * k)
                    };
                    let hw = half * base * (1.0 - rise).powf(0.7);
                    if hw < 0.3 {
                        continue;
                    }
                    let root_in = {
                        let k = (rise / 0.14).clamp(0.0, 1.0);
                        k * k * (3.0 - 2.0 * k)
                    };
                    let tip = (1.0 - rise).powf(0.55);
                    slab_tongues[n_slab] = (xc, hw, root_in, tip, flame_light_ramp(rise));
                    n_slab += 1;
                }
                let active_tongues = &slab_tongues[..n_slab];
                if active_tongues.is_empty() {
                    y += h;
                    continue;
                }
                let at = |x: f32| -> (u8, u32) {
                    let (mut a, mut sum, mut rgb) = (0.0f32, 0.0f32, [0.0f32; 3]);
                    for &(xc, hw, root_in, tip, c) in active_tongues {
                        let d = ((x - xc) / hw).abs();
                        if d >= 1.0 {
                            continue;
                        }
                        let side = (1.0 - d * d).powf(1.6);
                        let ta = Self::FIRE_LIGHT_ALPHA * side * root_in * tip;
                        if ta <= 0.0 {
                            continue;
                        }
                        a = a.max(ta);
                        sum += ta;
                        for (k, v) in rgb.iter_mut().enumerate() {
                            *v += ta * ((c >> (16 - 8 * k)) & 0xff) as f32;
                        }
                    }
                    if a < 1.0 {
                        return (0, 0);
                    }
                    let a = a.min(cap_at(x, ym));
                    if a < 1.0 {
                        return (0, 0);
                    }
                    let ch = |k: usize| ((rgb[k] / sum).round() as u32).min(255);
                    ((a as u8).max(1), (ch(0) << 16) | (ch(1) << 8) | ch(2))
                };
                let mut prev = (x_lo, at(x_lo));
                for k in 1..=knots {
                    let x = (x_lo + k as f32 * knot).min(x_hi);
                    let here = at(x);
                    let (x0, (a0, c0)) = prev;
                    let (a1, c1) = here;
                    if a0 > 1 || a1 > 1 {
                        let (xi0, xi1) = (x0.round() as i32, x.round() as i32);
                        let (c0, c1) = (if a0 > 0 { c0 } else { c1 }, if a1 > 0 { c1 } else { c0 });
                        crate::effect_util::push_fx_ramp(
                            under,
                            geom,
                            xi0,
                            y as i32,
                            xi1 - xi0,
                            (y + h) as i32 - y as i32,
                            (premul_rgb(c0, a0.max(1)), a0.max(1)),
                            (premul_rgb(c1, a1.max(1)), a1.max(1)),
                        );
                    }
                    prev = (x, here);
                }
                y += h;
            }
        }
    }

    fn flame_light_headroom(
        text: &TextView<'_>,
        row: u16,
        (u, half): (f32, f32),
        root: f32,
        (chf, oyf): (f32, f32),
    ) -> f32 {
        let (c0, c1) = ((u - half).floor() as i32, (u + half).ceil() as i32 - 1);
        for r in (0..i32::from(row)).rev().take(4) {
            if (c0..=c1).any(|c| text.covers_cell(r, c)) {
                let foot = oyf + (r + 1) as f32 * chf;
                return (root - foot - Self::FIRE_LIGHT_HEADROOM * chf).max(0.0);
            }
        }
        f32::INFINITY
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "config, text view, cell coordinates, paint bounds and per-pass memo slots"
    )]
    fn flame_light_cap(
        &self,
        cfg: &GlowConfig,
        text: &TextView<'_>,
        row: i32,
        col: i32,
        paints: (u32, u32),
        unknown_cap: &std::cell::Cell<Option<f32>>,
        pair_memo: &std::cell::Cell<Option<((u32, u32), f32)>>,
    ) -> f32 {
        let (fg, bg) = (cfg.theme_fg & 0x00FF_FFFF, cfg.theme_bg & 0x00FF_FFFF);
        if (text.knows_row(row) && !text.covers_cell(row, col)) || self.light_body_unsolved() {
            return Self::FIRE_LIGHT_ALPHA;
        }
        let table = crate::color_math::relative_luminance_table();
        let hold = |ink: u32, ground: u32, bar: f32| -> f32 {
            let ink_luma = crate::color_math::relative_luminance_with(table, ink);
            let ground_luma = crate::color_math::relative_luminance_with(table, ground);
            let orig_ratio =
                (ink_luma.max(ground_luma) + 0.05) / (ink_luma.min(ground_luma) + 0.05);
            let bar = bar.min(orig_ratio);
            let dark_ink = ink_luma <= ground_luma;
            let paint_end = if dark_ink { paints.0 } else { paints.1 };
            let holds = |a: f32| {
                if a < 1.0 {
                    return true;
                }
                let end_rgb = aterm_render::over_rgb(ground, paint_end, a as u8);
                let extreme = if dark_ink {
                    chan_min(ground, end_rgb)
                } else {
                    chan_max(ground, end_rgb)
                };
                let bound = |sh: u32| {
                    let c = (extreme >> sh) & 0xff;
                    if dark_ink {
                        c.saturating_sub(4) as usize
                    } else {
                        (c + 4).min(255) as usize
                    }
                };
                let luma =
                    crate::color_math::relative_luminance_rgb(table, bound(16), bound(8), bound(0));
                if dark_ink {
                    luma + 0.05 >= bar * (ink_luma + 0.05)
                } else {
                    ink_luma + 0.05 >= bar * (luma + 0.05)
                }
            };
            if holds(Self::FIRE_LIGHT_ALPHA) {
                return Self::FIRE_LIGHT_ALPHA;
            }
            let (mut lo, mut hi) = (0.0f32, Self::FIRE_LIGHT_ALPHA);
            for _ in 0..8 {
                let mid = 0.5 * (lo + hi);
                if holds(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        if !text.knows_row(row) {
            return match unknown_cap.get() {
                Some(cap) => cap,
                None => {
                    let cap = hold(fg, bg, 4.5).min(hold(Self::FIRE_LIGHT_REF_RED, bg, 3.0));
                    unknown_cap.set(Some(cap));
                    cap
                }
            };
        }
        let pair = match self.frame_text.pair(row, col) {
            Some((Some(ink), ground)) => (ink, ground),
            Some((None, ground)) => (fg, ground),
            None => (fg, bg),
        };
        match pair_memo.get() {
            Some((prev, cap)) if prev == pair => cap,
            _ => {
                let cap = hold(pair.0, pair.1, 4.5);
                pair_memo.set(Some((pair, cap)));
                cap
            }
        }
    }

    /// Claim one water cell for this frame's newest-visible-owner pass.
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

    /// Emit water's own fluid wake (not the shared straight comet).
    pub(super) fn emit_water(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
    ) {
        self.water_samples.clear();
        if !matches!(cfg.style, GlowStyle::Water) || self.sparks.is_empty() {
            return;
        }
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let heat = self.heat.clamp(0.0, 1.0);
        let swell_rate = 6.0 + 6.0 * heat;
        let swell_amp = ch * (Self::WATER_SWELL.0 + Self::WATER_SWELL.1 * heat);

        if self.water_seen.len() == Self::WATER_OWNER_SLOTS {
            self.water_seen.fill(0);
        } else {
            self.water_seen.clear();
            self.water_seen.resize(Self::WATER_OWNER_SLOTS, 0);
        }

        let mut head: Option<(u8, f32)> = None;
        for s in self.sparks.iter().rev() {
            if (s.row as usize) >= geom.rows || (s.col as usize) >= geom.cols {
                continue;
            }
            if cur == Some((s.row, s.col)) {
                continue;
            }
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            let u = (age / s.life).clamp(0.0, 1.0);
            let env = (1.0 - u) * (1.0 - 0.45 * u);
            let base_cov =
                (f32::from(s.born_cov) * env * cfg.intensity * 0.85).min(Self::WATER_WAKE_COV_CAP);

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
            let phase = s.col as f32 * 0.82 + s.row as f32 * 0.41 - age * swell_rate;
            let amp = swell_amp;
            let wave = phase.sin() * amp;
            let (sx, sy) = geom.cell_center(s.row, s.col);
            let line_y = sy + ch * Self::WATER_LINE_DY;
            let drift = (f32::from((seed >> 24) as u8) / 255.0 - 0.5) * cw * 0.36;
            let sample = WaterSample {
                surface: BeamVertex {
                    x: sx,
                    y: line_y + wave,
                    color: Self::water_crest_color(s.pos, heat),
                    cov: surface_cov,
                },
                phase,
                amp,
                line_y,
                row: s.row,
                col: s.col,
                drip_cov,
                drip_x: sx + drift,
                drip_y: sy + wave + ch * (0.08 + 2.15 * drip_t * drip_t),
                drip_h: ch * (0.08 + 0.30 * drip_t),
            };
            if head.is_none() && surface_cov > 0 {
                head = Some((surface_cov, age));
            }
            self.water_samples.push(sample);
        }
        self.water_samples.reverse();

        if let (Some((row, col)), Some((cov, age))) = (cur, head)
            && (row as usize) < geom.rows
            && (col as usize) < geom.cols
        {
            let phase = col as f32 * 0.82 + row as f32 * 0.41 - age * swell_rate;
            let amp = swell_amp;
            let (hx, hy) = geom.cell_center(row, col);
            let line_y = hy + ch * Self::WATER_LINE_DY;
            self.water_samples.push(WaterSample {
                surface: BeamVertex {
                    x: hx,
                    y: line_y + phase.sin() * amp,
                    color: water_ramp(0.80 + 0.12 * heat),
                    cov,
                },
                phase,
                amp,
                line_y,
                row,
                col,
                drip_cov: 0,
                drip_x: hx,
                drip_y: hy,
                drip_h: 0.0,
            });
        }

        let under_thickness = ch * (0.28 + 0.13 * heat);
        let crest_thickness = ch * (0.10 + 0.09 * heat);
        if !cfg.dark_theme {
            self.emit_water_light(cfg, geom, (under_thickness, crest_thickness), out);
            return;
        }
        let page = Self::bed_pair(cfg).1;
        let water_34 = water_ramp(0.34);
        for w in &self.water_samples {
            let v = w.surface;
            let undertow = premul_rgb(water_34, ((v.cov as f32) * 0.34) as u8);
            let lit = aterm_render::add_sat(
                aterm_render::add_sat(page, premul_rgb(v.color, v.cov)),
                undertow,
            );
            self.ink_ledger
                .lay_under((i32::from(w.row), i32::from(w.col)), lit);
        }

        if self.water_samples.len() == 1 && self.water_samples[0].surface.cov > 0 {
            let v = self.water_samples[0].surface;
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

        let mut curve = std::mem::take(&mut self.water_run);
        let clip = geom.beam_clip();
        let n = self.water_samples.len();
        let mut end = n;
        while end > 1 {
            let start = Self::water_run_start(&self.water_samples, end);
            let run = &self.water_samples[start..end];
            end = start;
            if run.len() < 2 || run.iter().all(|s| s.surface.cov == 0) {
                continue;
            }
            if out.len() >= Self::MAX_QUADS {
                break;
            }
            Self::water_curve(run, &mut curve);
            let surface_n = curve.len();
            curve.extend_from_within(..surface_n);
            for v in &mut curve[surface_n..] {
                v.cov = ((v.cov as f32) * 0.34) as u8;
                v.color = water_34;
            }
            comet_beam(out, clip, &curve[surface_n..], under_thickness, 1, 0.0);
            if out.len() >= Self::MAX_QUADS {
                break;
            }
            comet_beam(out, clip, &curve[..surface_n], crest_thickness, 1, 0.0);
        }
        self.water_run = curve;
        if out.len() >= Self::MAX_QUADS {
            out.truncate(Self::MAX_QUADS);
            return;
        }

        let drip_r = (cw * 0.16).max(1.25);
        let water_88 = water_ramp(0.88);
        for sample in self.water_samples.iter().rev() {
            if sample.drip_cov == 0 {
                continue;
            }
            if out.len() >= Self::MAX_QUADS {
                out.truncate(Self::MAX_QUADS);
                return;
            }
            crate::effect_util::push_soft_disc(
                out,
                geom,
                (sample.drip_x, sample.drip_y),
                (
                    drip_r * Self::WATER_DROP_SOFT,
                    (sample.drip_h * 0.5).max(drip_r) * Self::WATER_DROP_SOFT,
                ),
                water_88,
                f32::from(sample.drip_cov),
            );
        }
        out.truncate(Self::MAX_QUADS);
    }

    /// The water's crest colour at path position `pos` (tail 0, head 1) and heat `heat`.
    #[inline]
    pub(super) fn water_crest_color(pos: f32, heat: f32) -> u32 {
        water_ramp(0.66 + 0.24 * pos + 0.10 * heat)
    }

    #[inline]
    const fn water_run_start(samples: &[WaterSample], end: usize) -> usize {
        let mut start = end - 1;
        while start > 0 {
            let (a, b) = (&samples[start - 1], &samples[start]);
            if a.row.abs_diff(b.row) > 1 || a.col.abs_diff(b.col) > 1 {
                break;
            }
            start -= 1;
        }
        start
    }

    /// One run's surface as a curve into `curve` (cleared first).
    pub(super) fn water_curve(run: &[WaterSample], curve: &mut Vec<BeamVertex>) {
        curve.clear();
        let (Some(first), Some(last)) = (run.first(), run.last()) else {
            return;
        };
        let one_row = run.iter().all(|s| s.row == first.row);
        let n = run.len();
        for (i, pair) in run.windows(2).enumerate() {
            let (a, b) = (&pair[0], &pair[1]);
            for k in 0..Self::WATER_KNOTS {
                let t = k as f32 / Self::WATER_KNOTS as f32;
                let x = a.surface.x + (b.surface.x - a.surface.x) * t;
                let y = if one_row {
                    let line = a.line_y + (b.line_y - a.line_y) * t;
                    let phase = a.phase + (b.phase - a.phase) * t;
                    line + phase.sin() * (a.amp + (b.amp - a.amp) * t)
                } else {
                    let u = (i as f32 + t) / (n - 1) as f32;
                    first.line_y + (last.line_y - first.line_y) * u
                };
                let cov = f32::from(a.surface.cov)
                    + (f32::from(b.surface.cov) - f32::from(a.surface.cov)) * t;
                curve.push(BeamVertex {
                    x,
                    y,
                    color: if t < 0.5 {
                        a.surface.color
                    } else {
                        b.surface.color
                    },
                    cov: cov.round() as u8,
                });
            }
        }
        curve.push(BeamVertex {
            y: if one_row { last.surface.y } else { last.line_y },
            ..last.surface
        });
    }

    fn emit_water_light(
        &mut self,
        cfg: &GlowConfig,
        geom: Geom,
        (under_th, crest_th): (f32, f32),
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let ink = InkRole::Leading;
        let ground = self.light_ground(cfg);
        let opacity = |cov: u8| -> u8 {
            (f32::from(cov) * Self::LIGHT_WATER_ALPHA_CAP / Self::WATER_WAKE_COV_CAP)
                .min(Self::LIGHT_WATER_ALPHA_CAP) as u8
        };
        let undertow_ink = ground.body_ink(water_ramp(0.34));
        if self.water_samples.len() == 1 && self.water_samples[0].surface.cov > 0 {
            let v = self.water_samples[0].surface;
            let a = opacity(v.cov);
            let tw = (cw * 0.4).round().max(2.0) as i32;
            let th = (ch * 0.08).round().max(1.0) as i32;
            let (x, y) = (v.x.round() as i32 - tw / 2, v.y.round() as i32);
            push_fx_quad(out, geom, x, y, tw, th, premul_rgb(ink.ink(v.color), a), a);
        }
        let mut curve = std::mem::take(&mut self.water_run);
        let clip = geom.beam_clip();
        let n = self.water_samples.len();
        let mut end = n;
        let mut body_ink_memo: Option<(u32, u32)> = None;
        while end > 1 {
            let start = Self::water_run_start(&self.water_samples, end);
            let run = &self.water_samples[start..end];
            end = start;
            if run.len() < 2 || run.iter().all(|s| s.surface.cov == 0) {
                continue;
            }
            if out.len() >= Self::MAX_QUADS {
                break;
            }
            Self::water_curve(run, &mut curve);
            let surface_n = curve.len();
            curve.extend_from_within(..surface_n);
            for v in &mut curve[surface_n..] {
                v.color = undertow_ink;
                v.cov = (f32::from(opacity(v.cov)) * 0.34) as u8;
            }
            comet_beam_over(out, clip, &curve[surface_n..], under_th, 1, 0.0);
            for v in &mut curve[..surface_n] {
                v.color = match body_ink_memo {
                    Some((prev, solved)) if prev == v.color => solved,
                    _ => {
                        let solved = ground.body_ink(v.color);
                        body_ink_memo = Some((v.color, solved));
                        solved
                    }
                };
                v.cov = opacity(v.cov);
            }
            comet_beam_over(out, clip, &curve[..surface_n], crest_th, 1, 0.0);
        }
        self.water_run = curve;
        let drip_r = (cw * 0.16).max(1.25);
        for sample in self.water_samples.iter().rev() {
            if sample.drip_cov == 0 {
                continue;
            }
            if out.len() >= Self::MAX_QUADS {
                break;
            }
            let a = opacity((f32::from(sample.drip_cov) / 0.54).min(255.0) as u8);
            push_paper_bead(
                out,
                geom,
                (sample.drip_x, sample.drip_y),
                drip_r.max(sample.drip_h * 0.3),
                Self::LIGHT_WATER_DROP_INKS,
                a,
            );
        }
        out.truncate(Self::MAX_QUADS);
    }

    /// Emit the fire meteors as one straight anti-aliased streak per jump vector.
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
        let dark = cfg.dark_theme;
        let beam = if dark { comet_beam } else { comet_beam_over };
        let ramp_at = |r: f32| {
            if dark {
                r
            } else {
                Self::LIGHT_METEOR_RAMP.0
                    + (Self::LIGHT_METEOR_RAMP.1 - Self::LIGHT_METEOR_RAMP.0) * r
            }
        };
        const METEOR_COV_CAP: f32 = 185.0;
        let cov_cap = if dark {
            METEOR_COV_CAP
        } else {
            Self::LIGHT_METEOR_ALPHA_CAP
        };
        const SEG: [(f32, f32, f32, f32, f32); 3] = [
            (0.0, 0.45, 0.28, 0.16, 0.28),
            (0.45, 0.80, 0.55, 0.55, 0.55),
            (0.80, 1.0, 1.0, 1.0, 0.88),
        ];
        let clip = geom.beam_clip();
        let aura_c0 = fire_ramp(ramp_at(0.18));
        let aura_c1 = fire_ramp(ramp_at(0.35));
        let seg_colors = SEG
            .map(|(_, _, _, _, ramp)| (fire_ramp(ramp_at(ramp * 0.8)), fire_ramp(ramp_at(ramp))));
        for m in &self.fire_meteors {
            let age = now.saturating_duration_since(m.born).as_secs_f32();
            let u = (age / m.life).clamp(0.0, 1.0);
            let fade = 1.0 - u * u;
            if fade <= 0.02 {
                continue;
            }
            let tx = m.x0 + (m.x1 - m.x0) * u;
            let ty = m.y0 + (m.y1 - m.y0) * u;
            let (dx, dy) = (m.x1 - tx, m.y1 - ty);
            let (tx, dx) = if dy.abs() > 0.5 && dx.abs() > 1.6 * dy.abs() {
                let ndx = dx.signum() * 1.6 * dy.abs();
                (m.x1 - ndx, ndx)
            } else {
                (tx, dx)
            };
            if (dx * dx + dy * dy).sqrt() < 1.0 {
                continue;
            }
            let head_cov = (cov_cap * fade * cfg.intensity).clamp(0.0, 255.0);
            if head_cov < 1.0 {
                continue;
            }
            let core_t = (chf * (0.16 + 0.14 * m.mom)).max(2.5);
            let aura = [
                BeamVertex {
                    x: tx,
                    y: ty,
                    color: aura_c0,
                    cov: (head_cov * 0.06) as u8,
                },
                BeamVertex {
                    x: m.x1,
                    y: m.y1,
                    color: aura_c1,
                    cov: (head_cov * 0.16) as u8,
                },
            ];
            beam(out, clip, &aura, core_t * 1.5, 2, 0.0);
            if out.len() >= Self::MAX_QUADS {
                out.truncate(Self::MAX_QUADS);
                return;
            }
            let len = (dx * dx + dy * dy).sqrt();
            let feather = (Self::FIRE_METEOR_FEATHER * geom.cw as f32 / len).min(0.15);
            let snap_joins = true;
            #[cfg(test)]
            let (snap_joins, feather) = if self.fire_meteor_joined {
                (false, 0.0)
            } else {
                (snap_joins, feather)
            };
            let (m0, dm) = if dy.abs() >= dx.abs() {
                (ty, dy)
            } else {
                (tx, dx)
            };
            let join = |t: f32| -> f32 {
                if !snap_joins || t <= 0.0 || t >= 1.0 || dm.abs() < 1.0 {
                    return t;
                }
                (((m0 + dm * t).round() - m0) / dm).clamp(0.0, 1.0)
            };
            let head_from = if snap_joins {
                (1.0 - feather - Self::FIRE_METEOR_HEAD_EASE * geom.cw as f32 / len)
                    .clamp(SEG[1].0 + 0.05, SEG[2].0)
            } else {
                SEG[2].0
            };
            let bounds = [SEG[0].0, SEG[1].0, head_from, 1.0];
            let mut last: Option<(f32, u8, u32)> = None;
            for (i, (&(_, _, thick, covx, _), &(col0, col1))) in
                SEG.iter().zip(&seg_colors).enumerate()
            {
                if out.len() >= Self::MAX_QUADS {
                    return;
                }
                let c1 = (head_cov * covx) as u8;
                if c1 == 0 {
                    continue;
                }
                let (t0, t1) = (bounds[i], bounds[i + 1]);
                let w1 = (core_t * thick).max(1.5);
                let (w0, c0, col0) = match last {
                    Some(ended) if snap_joins => ended,
                    _ => (w1, (head_cov * covx * 0.55) as u8, col0),
                };
                let at = |t: f32, color: u32, cov: u8| BeamVertex {
                    x: tx + dx * t,
                    y: ty + dy * t,
                    color,
                    cov,
                };
                let head = t1 >= 1.0;
                let end = if head { 1.0 - feather } else { t1 };
                let steps = if snap_joins {
                    ((end - t0) * dm.abs() * 0.5).ceil().clamp(1.0, 8.0) as usize
                } else {
                    1
                };
                let cov_at = |f: f32| (f32::from(c0) + (f32::from(c1) - f32::from(c0)) * f) as u8;
                for k in 0..steps {
                    let (a, b) = (k as f32 / steps as f32, (k + 1) as f32 / steps as f32);
                    let (ta, tb) = (join(t0 + (end - t0) * a), join(t0 + (end - t0) * b));
                    if tb <= ta {
                        continue;
                    }
                    let width = w0 + (w1 - w0) * 0.5 * (a + b);
                    let verts = [
                        at(ta, lerp_rgb(col0, col1, a), cov_at(a)),
                        at(tb, lerp_rgb(col0, col1, b), cov_at(b)),
                    ];
                    beam(out, clip, &verts, width, 1, 0.0);
                }
                if head && feather > 0.0 {
                    let verts = [at(join(end), col1, c1), at(1.0, col1, 0)];
                    beam(out, clip, &verts, w1, 1, 0.0);
                }
                last = Some((w1, c1, col1));
                if out.len() >= Self::MAX_QUADS {
                    out.truncate(Self::MAX_QUADS);
                    return;
                }
            }
        }
    }

    /// Polyline scratch lent to v2 ([`rk::Frame::beams`]).
    pub(super) const V2_BEAM_SCRATCH: usize = 512;
    /// The cue sink v2 is lent ([`rk::Frame::cues`]).
    pub(super) const V2_CUE_SCRATCH: usize = 64;

    /// Size the three pixel streams to their caps once, on the first engaged frame: their peaks
    /// depend on state, so growing on demand could allocate long after warm-up.
    #[inline]
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

    /// Cap the halo stream to [`Self::MAX_HALOS`]; v2's quads are self-capped at the emitter.
    #[inline]
    pub(super) fn cap_and_clear_v2_streams(halos: &mut Vec<RainHalo>) {
        halos.truncate(Self::MAX_HALOS);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "clock, config, geometry, cursor, and the halo and under-ink streams; a param struct would obscure the single internal call site"
    )]
    pub(super) fn emit_crown(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        halos: &mut Vec<RainHalo>,
        under: &mut Vec<GlowQuad>,
    ) {
        if cfg.radius <= 0.0
            || !cfg
                .style
                .reads_knob(TrailKnob::Radius, KnobContext::widest(false))
        {
            return;
        }
        if !self.crown_until.is_some_and(|until| now < until) {
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
        let size = cfg.radius / GLOW_RADIUS_DEFAULT;
        if !cfg.dark_theme {
            if matches!(cfg.style, GlowStyle::Lumen) {
                let warmth = 0.6 + 0.4 * self.typing_boost().min(1.0);
                let cap = LIGHT_BODY_ALPHA_CAP;
                let peak = (cap * (0.55 + 0.45 * fade) * cfg.intensity * warmth).min(cap);
                let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
                let budget = under.len().saturating_add(Self::MAX_QUADS);
                push_ink_ellipse(
                    under,
                    geom,
                    geom.cell_center(cr, cc),
                    (cwf * 1.6 * size, cwf * 0.5 * size),
                    chf * 0.46 * size,
                    self.light_ground(cfg).body_ink(cfg.color),
                    peak,
                    budget,
                );
            }
            return;
        }
        let cw = geom.cw as i32;
        let ch = geom.ch as i32;
        let cx = geom.origin_x as i32 + cc as i32 * cw;
        let cy = geom.origin_y as i32 + cr as i32 * ch;
        let r = if matches!(cfg.style, GlowStyle::Fire) {
            (cfg.radius * geom.ch as f32 * (1.1 + 0.7 * self.fire_t())) as i32
        } else {
            (cfg.radius * geom.ch as f32) as i32
        };
        let (base_cov, color) = match cfg.style {
            GlowStyle::Laser => (105.0f32, cfg.color),
            GlowStyle::Fire => (
                (30.0 + 26.0 * self.fire_t()) * (0.74 + 0.26 * (age_ms as f32 * 0.045).sin()),
                fire_ramp(0.55 + 0.30 * self.fire_t()),
            ),
            GlowStyle::Water => (28.0, water_ramp(0.74)),
            GlowStyle::Phaser => (50.0, hsv2rgb(self.hue, 0.9, 1.0)),
            GlowStyle::Sparkle => (50.0, Self::sparkle_gold(cfg)),
            GlowStyle::Classic => (50.0, hsv2rgb(self.hue, 0.9, 1.0)),
            GlowStyle::RainbowKitty => return,
            GlowStyle::Beam => (46.0, cfg.color),
            GlowStyle::Lumen => (50.0, cfg.color),
            GlowStyle::Comet => (44.0, lerp_rgb(cfg.color, 0x00FF_FFFF, 0.30)),
        };
        let boost = if matches!(cfg.style, GlowStyle::Laser) {
            self.typing_boost().max(1.3 * self.blaze())
        } else if matches!(cfg.style, GlowStyle::Fire) {
            (0.28 + 0.92 * self.fire_t()).max(1.2 * self.fire_t())
        } else {
            self.typing_boost()
        };
        if matches!(cfg.style, GlowStyle::Fire) {
            let (ccx, ccy) = (cx as f32 + cw as f32 * 0.5, cy as f32 - ch as f32 * 0.10);
            let reach = r as f32 + ch as f32 * 0.55;
            let peak = (base_cov * fade * cfg.intensity * boost).min(255.0);
            if peak >= 1.0 {
                // THE CARET'S BLOOM BURNS EMBER: a deep orange-red over the
                // dark, its heart an orange. Lit from the ramp's gold and
                // cream, a bloom this dim added a tan to the page's blue-grey
                // and read khaki round the caret (#5f4b2b to #2e271d,
                // 2026-10-23, `burst`), no fire's colour.
                #[cfg(test)]
                let unwarmed = self.fire_bloom_unwarmed;
                #[cfg(not(test))]
                let unwarmed = false;
                let (color, heart) = if unwarmed {
                    (color, fire_ramp(0.62 + 0.23 * self.fire_t()))
                } else {
                    (
                        fire_ramp(Self::FIRE_BLOOM_EMBER),
                        fire_ramp(Self::FIRE_BLOOM_HEART + 0.06 * self.fire_t()),
                    )
                };
                for (radii, rgb, cov) in [
                    ((reach * 1.9, reach * 1.45), color, peak * 0.34),
                    ((reach * 1.05, reach * 0.85), heart, peak * 0.82),
                ] {
                    let share = self.ink_ledger.admit_halo(
                        (ccx, ccy),
                        radii,
                        premul_rgb(rgb, cov as u8),
                        InkLedger::STEADY_FLOOR,
                    );
                    push_halo(
                        halos,
                        geom,
                        ccx,
                        ccy,
                        radii.0,
                        radii.1,
                        premul_rgb(rgb, (cov * share) as u8),
                    );
                }
            }
            return;
        }
        let (ccx, ccy) = (cx as f32 + cw as f32 * 0.5, cy as f32 + ch as f32 * 0.5);
        let peak = (base_cov * 1.9 * fade * cfg.intensity * boost).min(210.0);
        if peak >= 1.0 {
            let from = halos.len();
            let (core_rx, core_ry) = (cw as f32 * 1.25 * size, ch as f32 * 0.85 * size);
            let laser = matches!(cfg.style, GlowStyle::Laser);
            let (core_rx, core_ry) = if laser {
                hold_glow_radii_in_box(geom, (ccx, ccy), (core_rx, core_ry))
            } else {
                (core_rx, core_ry)
            };
            push_halo(
                halos,
                geom,
                ccx,
                ccy,
                core_rx,
                core_ry,
                premul_rgb(color, peak as u8),
            );
            if laser {
                let (rx, ry) = hold_glow_radii_in_box(
                    geom,
                    (ccx, ccy),
                    (cw as f32 * 0.7 * size, ch as f32 * 0.55 * size),
                );
                push_halo(
                    halos,
                    geom,
                    ccx,
                    ccy,
                    rx,
                    ry,
                    premul_rgb(color, (peak * 0.9).min(230.0) as u8),
                );
            }
            self.ink_ledger
                .admit_halos_masked(halos, from, InkLedger::HEAD_FLOOR);
        }
    }

    /// The forge dressing on the cursor cell (fire only).
    pub(super) fn emit_forge_cursor(
        &mut self,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        if !matches!(cfg.style, GlowStyle::Fire) || !cfg.dark_theme {
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
        let (ccx, ccy) = (cx as f32 + cw as f32 * 0.5, cy as f32 - ch as f32 * 0.10);
        let tn = ((temp - Self::FORGE_MIN_TEMP) / (1.0 - Self::FORGE_MIN_TEMP)).clamp(0.0, 1.0);
        let phase = self.flame_phase;
        let pulse = 0.85 + 0.15 * (phase * 2.3).sin();
        let core_cov = (150.0 * tn * pulse * cfg.intensity).min(255.0);
        if core_cov >= 2.0 {
            let (centre, radii) = (
                (ccx, ccy - ch as f32 * 0.12),
                (
                    cw as f32 * (0.30 + 0.14 * tn),
                    ch as f32 * (0.20 + 0.10 * tn),
                ),
            );
            let rgb = fire_ramp((0.72 + 0.20 * tn).min(0.92));
            let share = self.ink_ledger.admit_halo(
                centre,
                radii,
                premul_rgb(rgb, core_cov as u8),
                InkLedger::STEADY_FLOOR,
            );
            push_halo(
                halos,
                geom,
                centre.0,
                centre.1,
                radii.0,
                radii.1,
                premul_rgb(rgb, (core_cov * share) as u8),
            );
        }
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
        if temp > 0.5 {
            let crawl_cov = (130.0 * (temp - 0.5) * 2.0 * cfg.intensity).min(200.0);
            if crawl_cov >= 2.0 {
                let per = 2.0 * (cw as f32 + ch as f32);
                let rgb = fire_ramp(0.85);
                let premul = premul_rgb(rgb, crawl_cov as u8);
                for k in 0..2u32 {
                    let u = fract(phase * (0.23 + 0.07 * k as f32) + k as f32 * 0.5);
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
                    let share = self.ink_ledger.admit_halo(
                        (ex, ey),
                        (3.5, 3.5),
                        premul,
                        InkLedger::STEADY_FLOOR,
                    );
                    push_halo(
                        halos,
                        geom,
                        ex,
                        ey,
                        3.5,
                        3.5,
                        premul_rgb(rgb, (crawl_cov * share) as u8),
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
        cur: Option<(u16, u16)>,
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
        let rf = classic_ring_radius_factor(ring.scale);
        let color = match cfg.style {
            GlowStyle::Laser => cfg.color,
            // Fire on paper cools from white-hot as it dies (the dark
            // flash is all ember: [`Self::fire_dark_flash`]).
            GlowStyle::Fire => fire_ramp(0.95 - 0.35 * t),
            GlowStyle::Water => water_ramp(0.6),
            _ => cfg.accent,
        };
        if !cfg.dark_theme {
            self.emit_ring_light(cfg, geom, cur, ring, t, cov, color, out, halos);
            return;
        }
        // Fire: the impact is a brief round flash, an ember's orange-red
        // cooling to red ([`Self::fire_dark_flash`]) as it spreads to
        // [`Self::FIRE_DARK_LANDING_REACH`] of a row and dies by the square
        // of its life, with no aftershock: a bloom that spread to three cells
        // and an aftershock behind it stood as a flat orange disc where the
        // caret had landed, still there after a clear took the line away.
        // Every round bloom below is fitted inside the effects box, so a
        // landing at the grid's edge fades out before it instead of standing
        // cut by it ([`fit_glow_in_box`]).
        let bloom = |halos: &mut Vec<RainHalo>, radii: (f32, f32), premul: u32| {
            let ((cx, cy), (rx, ry)) = fit_glow_in_box(geom, (ring.cx, ring.cy), radii);
            push_halo(halos, geom, cx, cy, rx, ry, premul);
        };
        if matches!(cfg.style, GlowStyle::Fire) {
            let grow = (0.45 + (Self::FIRE_DARK_LANDING_REACH - 0.45) * t) * geom.ch as f32 * rf;
            let cov = (f32::from(cov) * fade) as u8;
            if cov > 0 {
                bloom(
                    halos,
                    (grow * 1.3, grow),
                    premul_rgb(Self::fire_dark_flash(t), cov),
                );
            }
            return;
        }
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
        let grow = (0.7 + 1.0 * t) * geom.ch as f32 * rf;
        let peak = (f32::from(cov) * Self::DARK_RING_SHARE) as u8;
        // THE DARK VEIL STAYS IN THE CARET'S CELL AND ROW, in the style's
        // clean hue, as the paper veil does ([`Self::emit_ring_light`]):
        // after an Enter at the bottom row the bloom, carried up with the
        // scrolled text, stood as a round grey haze two rows over the
        // caret on the lines just printed (2026-10-22, sparkle `#303236`,
        // comet `#2d3237`, phaser and lumen `#1f322f`, laser `#272437`: a
        // near-white accent added thin over the dark). It sits on the
        // caret's cell (the last one the ring saw while the host withholds
        // the caret; with none known, on the landing's own cell), reaches
        // along the row no further than [`Self::LIGHT_VEIL_REACH_CELLS`] and
        // never past the row's edges, and its centre's light is held to what
        // leaves the theme's text [`Self::LIGHT_VEIL_BAR`] over it.
        #[cfg(not(test))]
        let unbound = false;
        #[cfg(test)]
        let unbound = self.veil_unbound;
        if unbound {
            bloom(halos, (grow, grow * 0.85), premul_rgb(color, peak));
            return;
        }
        let centre = cur
            .filter(|&(r, c)| usize::from(r) < geom.rows && usize::from(c) < geom.cols)
            .or(ring.caret)
            .map_or((ring.cx, ring.cy), |(r, c)| geom.cell_center(r, c));
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let veil = Self::dark_veil_ink(cfg, color, self.colour_pins);
        let cap = Self::dark_veil_cap(veil, cfg.theme_fg, cfg.theme_bg);
        // Along its row the veil reaches farther the harder the landing
        // ([`classic_ring_radius_factor`]), so a capped-impact jump's still
        // spreads to twice a hop's, as the ring it replaced did.
        let radii = (
            grow.min(Self::LIGHT_VEIL_REACH_CELLS * cw * rf),
            (grow * 0.85).min(0.5 * ch),
        );
        let ((cx, cy), (rx, ry)) = fit_glow_in_box(geom, centre, radii);
        push_halo(halos, geom, cx, cy, rx, ry, premul_rgb(veil, peak.min(cap)));
    }

    /// A dark landing veil's light for the ring's `rgb`: the style's own
    /// hue (the ring's colour, or the trail's where the ring's is the
    /// near-white accent brightened from it, as Sparkle's gold) at a clean
    /// chroma and full value, so a thin veil over the dark reads as its
    /// colour, never the grey a near-white accent adds to (a grey trail
    /// colour stays grey). A colour the user chose ([`ColourPins`]) is the
    /// veil's as it is, held legible by [`Self::dark_veil_cap`]'s coverage
    /// alone: re-hued and brightened, a pinned white accent landed green and
    /// a pinned violet Laser `#9f40ff` (2026-10-23, review).
    pub(super) fn dark_veil_ink(cfg: &GlowConfig, rgb: u32, pins: ColourPins) -> u32 {
        let chosen = match cfg.style {
            GlowStyle::Laser => pins.trail,
            _ => pins.accent || (pins.trail && cfg.style.draws_trail_color()),
        };
        if chosen {
            return rgb & 0x00FF_FFFF;
        }
        let sat = |c: u32| crate::color_math::rgb2hsv(c & 0x00FF_FFFF).1;
        let rgb = if sat(rgb) < 0.15 && !pins.trail && sat(cfg.color) > sat(rgb) {
            cfg.color & 0x00FF_FFFF
        } else {
            rgb & 0x00FF_FFFF
        };
        let (h, s, _) = crate::color_math::rgb2hsv(rgb);
        if s < 0.04 {
            return rgb;
        }
        crate::color_math::hsv2rgb(h, s.max(Self::LIGHT_VEIL_SAT), 1.0)
    }

    /// The most coverage a dark landing veil in `veil` may add at its centre
    /// and leave the theme's text `fg` [`Self::LIGHT_VEIL_BAR`] over it on
    /// the page `bg` (or the theme's own contrast, if less: no veil).
    pub(crate) fn dark_veil_cap(veil: u32, fg: u32, bg: u32) -> u8 {
        let table = crate::color_math::relative_luminance_table();
        let (fg, bg) = (fg & 0x00FF_FFFF, bg & 0x00FF_FFFF);
        let lum_fg = crate::color_math::relative_luminance_with(table, fg);
        let lum_bg = crate::color_math::relative_luminance_with(table, bg);
        let contrast = |l: f32| (lum_fg.max(l) + 0.05) / (lum_fg.min(l) + 0.05);
        let bar = Self::LIGHT_VEIL_BAR.min(contrast(lum_bg));
        let (bg_r, bg_g, bg_b) = ((bg >> 16) & 0xff, (bg >> 8) & 0xff, bg & 0xff);
        let (v_r, v_g, v_b) = ((veil >> 16) & 0xff, (veil >> 8) & 0xff, veil & 0xff);
        let holds = |a: u32| {
            let r = (bg_r + (v_r * a + 127) / 255).min(255) as usize;
            let g = (bg_g + (v_g * a + 127) / 255).min(255) as usize;
            let b = (bg_b + (v_b * a + 127) / 255).min(255) as usize;
            contrast(crate::color_math::relative_luminance_rgb(table, r, g, b)) >= bar
        };
        let (mut lo, mut hi) = (0u32, 255u32);
        if holds(hi) {
            return 255;
        }
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if holds(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo as u8
    }

    /// How far a dark fire's landing flash spreads at the end of its life, in rows.
    pub(super) const FIRE_DARK_LANDING_REACH: f32 = 1.0;

    /// A dark fire's landing flash at `t` of its life: an ember's
    /// orange-red cooling to red. A bloom thins to nothing at its rim, and
    /// a thin yellow over the dark is no colour of fire: the white-hot
    /// flash (cooled to amber) stood around a jump's landing as a dim
    /// khaki disc (`#352b1c` on the gallery's dark, 2026-10-22). Orange-red
    /// stays warm however thin it lies.
    pub(super) fn fire_dark_flash(t: f32) -> u32 {
        fire_ramp(0.55 - 0.25 * t.clamp(0.0, 1.0))
    }

    /// The dark ring's bloom peak as a share of the ring's coverage: a
    /// filled bloom carries far more light than the 2 px outline it replaced
    /// at the same coverage. Held to the caret's cell and row, the veil's
    /// cap ([`Self::dark_veil_cap`]) bounds most hues' centre below this; a
    /// dim hue carries its light up to that cap, where at 0.45 Laser's
    /// violet veil barely showed past the caret block in the settings
    /// preview's card (7 px more than 32 levels from no ring, 2026-10-22).
    const DARK_RING_SHARE: f32 = 0.8;

    #[allow(
        clippy::too_many_arguments,
        reason = "the ring's resolved facts (life, coverage, colour) handed over from emit_ring"
    )]
    fn emit_ring_light(
        &self,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        ring: Ring,
        t: f32,
        cov: u8,
        color: u32,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        let rf = classic_ring_radius_factor(ring.scale);
        if matches!(cfg.style, GlowStyle::Water) {
            let cx = ring.cx;
            let cy = ring.cy + geom.ch as f32 * 0.28;
            let mut ripple = |tt: f32, cov: u8, color: u32| {
                if cov == 0 {
                    return;
                }
                let rx = (0.7 + 1.5 * tt) * geom.ch as f32 * rf;
                let ry = (rx * 0.38).max(2.0);
                let from = out.len();
                push_ripple_ring(
                    out,
                    geom,
                    (cx, cy),
                    (rx, ry),
                    2.0,
                    premul_rgb(0x00FF_FFFF, cov),
                );
                let ink = InkRole::OverText.ink(color);
                reink_quads(&mut out[from..], LIGHT_INK_ALPHA_CAP as u8, |_| ink);
            };
            ripple(t, cov, color);
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
        let grow = (0.7 + 1.0 * t) * geom.ch as f32 * rf;
        let veil = if matches!(cfg.style, GlowStyle::Fire) {
            Self::FIRE_LIGHT_LANDING
        } else {
            Self::light_veil_ink(color)
        };
        let cap = if matches!(cfg.style, GlowStyle::Fire) {
            ((cov as u32) / 2).clamp(1, 120) as u8
        } else {
            ((cov as u32) * 3 / 4).clamp(1, 160) as u8
        };
        #[cfg(not(test))]
        let unbound = false;
        #[cfg(test)]
        let unbound = self.veil_unbound;
        let on_grid = cur
            .filter(|&(r, c)| usize::from(r) < geom.rows && usize::from(c) < geom.cols)
            .or(ring.caret);
        let (centre, radii, cap) = match on_grid {
            Some((r, c)) if !unbound => {
                let (cw, ch) = (geom.cw as f32, geom.ch as f32);
                (
                    geom.cell_center(r, c),
                    (
                        grow.min(Self::LIGHT_VEIL_REACH_CELLS * cw),
                        (grow * 0.85).min(0.5 * ch),
                    ),
                    cap.min(Self::light_veil_cap(veil, cfg.theme_fg, cfg.theme_bg)),
                )
            }
            _ if unbound => ((ring.cx, ring.cy), (grow, grow * 0.85), cap),
            _ => return,
        };
        push_soft_veil(halos, geom, centre, radii, veil, cap);
    }

    /// Maximum centre opacity for a light landing veil in `veil` over `fg` and `bg`.
    pub(crate) fn light_veil_cap(veil: u32, fg: u32, bg: u32) -> u8 {
        let own = crate::color_math::contrast_ratio(fg & 0x00FF_FFFF, bg & 0x00FF_FFFF);
        let keep = (Self::LIGHT_VEIL_BAR / own.max(1.0)).min(1.0);
        light_alpha_keeping(veil, fg, bg, keep).clamp(0.0, 255.0) as u8
    }

    /// A light landing veil's ink for `rgb`.
    pub(super) fn light_veil_ink(rgb: u32) -> u32 {
        let (h, s, _) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
        let h = if (30.0..80.0).contains(&h) {
            30.0 + (h - 30.0) * 0.2
        } else if (80.0..95.0).contains(&h) {
            40.0 + (h - 80.0) * (55.0 / 15.0)
        } else {
            h
        };
        let full = crate::color_math::hsv2rgb(h, s.max(Self::LIGHT_VEIL_SAT), 1.0);
        let y = luma709(full);
        if y <= LIGHT_INK_BOLD_MAX_LUMA {
            return full;
        }
        let k = LIGHT_INK_BOLD_MAX_LUMA / y;
        let ch = |sh: u32| ((((full >> sh) & 0xff) as f32 * k) + 0.5) as u32;
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    }

    /// Emit the flying particles, style by style.
    pub(super) fn emit_particles(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
        under: &mut Vec<GlowQuad>,
    ) {
        if self.particles.is_empty() {
            return;
        }
        let sz = (geom.ch as f32 * 0.18).max(2.0) as i32;
        let water = matches!(cfg.style, GlowStyle::Water);
        let dark = cfg.dark_theme;
        let beam = matches!(cfg.style, GlowStyle::Beam);
        let lawful = !matches!(cfg.style, GlowStyle::Fire | GlowStyle::Beam);
        let light_fire = !dark && matches!(cfg.style, GlowStyle::Fire);
        let (cwf, ch) = (geom.cw as f32, geom.ch as f32);
        let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
        let text = TextView::new(self.probed_text_spans(), Some(&self.frame_text));
        let covers = |x: f32, y: f32| {
            let row = ((y - oyf) / ch).floor() as i32;
            let col = ((x - oxf) / cwf).floor() as i32;
            text.covers_cell(row, col)
        };
        let margin = ch * 0.14;
        let near_text = |x: f32, y: f32| {
            covers(x, y)
                || covers(x, y - margin)
                || covers(x, y + margin)
                || covers(x - margin, y)
                || covers(x + margin, y)
        };
        let glyph_band = |y: f32| -> Option<(i32, f32)> {
            let row = ((y - oyf) / ch).floor() as i32;
            let top = oyf + row as f32 * ch;
            let k = (y - top) / ch;
            ((Self::GLYPH_BAND.0..Self::GLYPH_BAND.1).contains(&k)
                && text.knows_row(row)
                && !text.row_is_blank(row))
            .then_some((row, top))
        };
        let caret_row = self.last.map(|(r, _)| i32::from(r));
        let caret = self.last.map(|(r, c)| Self::caret_box(geom, r, c));
        let reach = ch * 0.30;
        let on_caret = |x: f32, y: f32| {
            caret.is_some_and(|(x0, y0, x1, y1)| {
                x > x0 as f32 - reach
                    && x < x1 as f32 + reach
                    && y > y0 as f32 - reach
                    && y < y1 as f32 + reach
            })
        };
        let comet_dark_leading =
            dark && matches!(cfg.style, GlowStyle::Comet) && !self.streak_unbudgeted();
        let sparkle = matches!(cfg.style, GlowStyle::Sparkle);
        let base_cov = if water { 112.0 } else { 230.0 };
        let laser_thick = (ch * 0.11).max(1.5);
        let mini_comet_girth = if dark {
            (ch * 0.09).max(1.2)
        } else {
            (ch * 0.13).max(2.0)
        };
        let fine_star_arm = star_arm_px(star_arm(ch, STAR_ARM_FINE));
        let sparkle_gold_rgb = (sparkle && dark).then(|| Self::sparkle_gold(cfg));
        let laser_light_ink = if matches!(cfg.style, GlowStyle::Laser) && !dark {
            InkRole::OverText.ink(cfg.color)
        } else {
            0
        };
        let comet_light_ground =
            (matches!(cfg.style, GlowStyle::Comet) && !dark).then(|| self.light_ground(cfg));
        let mut comet_body_ink_memo: Option<(u32, u32)> = None;
        'flying: for p in self.particles.iter().rev() {
            if out.len() >= Self::MAX_QUADS {
                break 'flying;
            }
            let age = now.saturating_duration_since(p.born).as_secs_f32();
            let t = (age / p.life).clamp(0.0, 1.0);
            let fade = 1.0 - t;
            let x = p.x0 + p.vx * age;
            let y = p.y0 + p.vy * age + 0.5 * p.gy * age * age;
            let leading_band = comet_dark_leading.then(|| glyph_band(y)).flatten();
            let leading = leading_band.is_some();
            let seat = (p.hue * 977.0).fract();
            if leading && seat >= Self::COMET_DARK_DUST_SHARE {
                continue;
            }
            let y = match leading_band {
                Some((_, top)) => {
                    let k = seat / Self::COMET_DARK_DUST_SHARE;
                    let jitter = ((p.hue * 4513.0).fract() - 0.5) * Self::COMET_DARK_DUST_JITTER;
                    top + ch
                        * (jitter
                            + if k < 0.5 {
                                Self::COMET_DARK_DUST_HIGH
                            } else {
                                Self::COMET_DARK_DUST_LOW
                            })
                }
                None => y,
            };
            let over_text = (lawful || beam) && covers(x, y);
            if over_text && (!dark || sparkle || beam) {
                continue;
            }
            if (sparkle || beam)
                && (near_text(x, y)
                    || glyph_band(y).is_some_and(|(row, _)| {
                        let col = ((x - oxf) / cwf).floor() as i32;
                        Some(row) != caret_row && (-2..=2).any(|d| text.covers_cell(row, col + d))
                    }))
            {
                continue;
            }
            if (!dark || sparkle) && (lawful || light_fire) && on_caret(x, y) {
                continue;
            }
            let text_dim = if over_text && !water && !leading {
                Self::GRAIN_OVER_GLYPH_DIM
            } else {
                1.0
            };
            let particle_color = || {
                let hue = match cfg.style {
                    GlowStyle::Fire | GlowStyle::Water | GlowStyle::Comet => p.hue,
                    _ => (p.hue + age * 0.5).fract(),
                };
                style_particle_color(cfg.style, cfg.color, hue, fade)
            };
            let cov = (base_cov * p.cov_scale * fade * cfg.intensity * text_dim) as u8;
            if cov == 0 {
                continue;
            }
            let ink_alpha =
                |c: u8| (f32::from(c) * Self::LIGHT_GRAIN_GAIN).min(LIGHT_INK_ALPHA_CAP) as u8;
            if !dark && matches!(cfg.style, GlowStyle::Beam) {
                continue;
            }
            match cfg.style {
                GlowStyle::Laser => {
                    let thick = laser_thick;
                    let vy = p.vy + p.gy * age;
                    if dark {
                        push_velocity_dash(
                            out,
                            geom,
                            x,
                            y,
                            p.vx,
                            vy,
                            0.05,
                            thick,
                            cfg.color,
                            particle_color(),
                            cov,
                        );
                    } else {
                        push_velocity_dash_over(
                            out,
                            geom,
                            x,
                            y,
                            p.vx,
                            vy,
                            0.05,
                            thick,
                            laser_light_ink,
                            ink_alpha(cov),
                        );
                    }
                    continue;
                }
                GlowStyle::Beam => {
                    let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                    let scov = ((cov as f32) * twinkle).clamp(0.0, 255.0) as u8;
                    if scov > 0 {
                        let tint = if p.hue < 0.45 {
                            0x00FF_FFFF
                        } else if p.hue < 0.85 {
                            0x00CF_ECFF
                        } else {
                            0x00A9_8BFF
                        };
                        if !star_accent((p.hue * 4096.0) as u32) {
                            let r = dust_r(ch, p.hue) * (0.6 + 0.4 * fade);
                            push_dust_halo(halos, geom, x, y, r, tint, scov);
                            continue;
                        }
                        let arm = fine_star_arm;
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
                GlowStyle::Comet => {
                    let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                    let gcov = ((cov as f32) * twinkle) as u8;
                    if gcov == 0 {
                        continue;
                    }
                    if !dark && under.len() >= Self::MAX_QUADS {
                        continue;
                    }
                    let r = dust_r(ch, p.hue) * (0.6 + 0.4 * fade);
                    let rgb = particle_color();
                    if let Some(ground) = comet_light_ground {
                        let body_ink = match comet_body_ink_memo {
                            Some((prev, solved)) if prev == rgb => solved,
                            _ => {
                                let solved = ground.body_ink(rgb);
                                comet_body_ink_memo = Some((rgb, solved));
                                solved
                            }
                        };
                        let mote = r * STAR_ARM_INK;
                        let budget = under.len().saturating_add(Self::MAX_QUADS);
                        push_ink_ellipse(
                            under,
                            geom,
                            (x, y),
                            (mote, mote),
                            mote,
                            body_ink,
                            f32::from(ink_alpha(gcov)),
                            budget,
                        );
                        continue;
                    }
                    push_dust_halo(halos, geom, x, y, r, rgb, gcov);
                    let glint = star_accent((p.hue * 4096.0) as u32) && twinkle_peak(twinkle);
                    if !glint {
                        continue;
                    }
                    let glint_cov = (f32::from(gcov) * COMET_GLINT_COV) as u8;
                    if !push_twinkle_star(
                        out,
                        geom,
                        x as i32,
                        y as i32,
                        fine_star_arm,
                        glint_cov,
                        false,
                        0x00FF_FFFF,
                        Self::MAX_QUADS,
                    ) {
                        break 'flying;
                    }
                    continue;
                }
                GlowStyle::Fire => {
                    let flicker = 0.68
                        + 0.32 * (age * (9.0 + 7.0 * p.hue) + p.hue * std::f32::consts::TAU).sin();
                    let waft = (age * (2.0 + 3.0 * p.hue) + p.hue * std::f32::consts::TAU).sin()
                        * geom.cw as f32
                        * 0.6
                        * t;
                    if light_fire {
                        if covers(x + waft, y) {
                            continue;
                        }
                        let alpha = ((cov as f32) * flicker * 0.95).min(235.0) as u8;
                        let r = (ch * (0.06 + 0.06 * p.hue)).max(1.8) * (0.7 + 0.3 * fade);
                        push_paper_bead(out, geom, (x + waft, y), r, Self::LIGHT_EMBER_INKS, alpha);
                        continue;
                    }
                    let ccov_f = (cov as f32) * flicker * Self::FIRE_EMBER_COV;
                    if ccov_f < 1.0 {
                        continue;
                    }
                    let cell = geom.ch as f32;
                    let base = (cell * (Self::FIRE_EMBER_R + Self::FIRE_EMBER_R_HOT * p.hue))
                        .max(1.2)
                        * (0.6 + 0.4 * fade);
                    let vx = p.vx;
                    let vy = p.vy + p.gy * age;
                    let rx = base * (1.0 + (vx.abs() / cell).min(1.4) * 0.6);
                    let ry = base * (1.0 + (vy.abs() / cell).min(1.4) * 0.6);
                    let core_color = fire_ramp((0.42 + 0.30 * fade + 0.18 * p.hue).min(0.92));
                    let (centre, radii) = ((x + waft, y), (rx * 1.6, ry * 1.6));
                    let reach = radii.0.max(radii.1);
                    let near = |dx: f32, dy: f32| covers(centre.0 + dx, centre.1 + dy);
                    if !self.streak_unbudgeted()
                        && (near(0.0, 0.0)
                            || near(-reach, 0.0)
                            || near(reach, 0.0)
                            || near(0.0, -reach)
                            || near(0.0, reach)
                            || on_caret(centre.0, centre.1))
                    {
                        continue;
                    }
                    let share = self.ink_ledger.admit_halo(
                        centre,
                        radii,
                        premul_rgb(core_color, ccov_f as u8),
                        InkLedger::FLOOR,
                    );
                    let ccov = (ccov_f * share) as u8;
                    if ccov == 0 {
                        continue;
                    }
                    push_halo(
                        halos,
                        geom,
                        centre.0,
                        centre.1,
                        radii.0,
                        radii.1,
                        premul_rgb(core_color, ccov),
                    );
                    continue;
                }
                GlowStyle::Sparkle => {
                    let seed = (p.hue * 4096.0) as u32;
                    let (ix, iy) = (x as i32, y as i32);
                    let (lift, grow) = if dark {
                        (Self::SPARKLE_DARK_LIFT, Self::SPARKLE_DARK_GROW)
                    } else {
                        (1.0, 1.0)
                    };
                    let cov = (f32::from(cov) * lift).min(255.0) as u8;
                    match seed % 8 {
                        0 if star_accent(seed) => {
                            let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                            let scov = ((cov as f32) * twinkle).clamp(0.0, 255.0) as u8;
                            if scov > 0 {
                                let ratio = STAR_ARM_FINE + (STAR_ARM_STD - STAR_ARM_FINE) * p.hue;
                                if !dark {
                                    rk::stardust::push_twinkle_over_twin(
                                        halos,
                                        geom,
                                        Self::MAX_HALOS,
                                        rk::stardust::LightStar {
                                            centre: (ix, iy),
                                            mass: star_arm(ch, ratio) * STAR_ARM_INK,
                                            step: 0,
                                            rgb: Self::SPARKLE_PAPER_STAR,
                                            gold: false,
                                            cov: Self::paper_gold_alpha(scov),
                                        },
                                    );
                                    continue;
                                }
                                let arm = star_arm_px(star_arm(ch, ratio));
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
                            let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                            let scov = ((cov as f32) * twinkle).clamp(0.0, 255.0) as u8;
                            if scov > 0 {
                                let r = dust_r(ch, p.hue) * (0.6 + 0.4 * fade) * grow;
                                if dark {
                                    push_dust_halo(halos, geom, x, y, r, particle_color(), scov);
                                } else {
                                    push_paper_bead(
                                        out,
                                        geom,
                                        (x, y),
                                        r * Self::SPARKLE_PAPER_BEAD,
                                        (Self::SPARKLE_PAPER_RIM, Self::SPARKLE_PAPER_CORE),
                                        Self::paper_gold_alpha(scov),
                                    );
                                }
                            }
                            continue;
                        }
                        4 | 5 => {
                            let r = (sz as f32 * 0.5).max(1.3);
                            if dark {
                                push_halo(
                                    halos,
                                    geom,
                                    x,
                                    y,
                                    r * grow,
                                    r * grow,
                                    premul_rgb(sparkle_gold_rgb.unwrap_or(0), cov),
                                );
                            } else {
                                push_paper_bead(
                                    out,
                                    geom,
                                    (x, y),
                                    r * Self::SPARKLE_PAPER_BEAD,
                                    (Self::SPARKLE_PAPER_RIM, Self::SPARKLE_PAPER_CORE),
                                    Self::paper_gold_alpha(cov),
                                );
                            }
                            continue;
                        }
                        6 => {
                            if over_text {
                                continue;
                            }
                            let r = (Self::MOON_DUST_RATIO * dust_r(ch, p.hue)).max(2.5);
                            let moon = (230.0
                                * p.cov_scale
                                * fade.powf(Self::MOON_FADE_POW)
                                * cfg.intensity
                                * lift)
                                .min(255.0) as u8;
                            if dark {
                                push_crescent(out, geom, (x, y), r, Self::MOON_GOLD, moon, false);
                            } else {
                                push_crescent(
                                    out,
                                    geom,
                                    (x, y),
                                    r,
                                    Self::SPARKLE_PAPER_MOON,
                                    Self::paper_gold_alpha(moon),
                                    true,
                                );
                            }
                            continue;
                        }
                        _ => {
                            let (thick, vy) = (mini_comet_girth, p.vy + p.gy * age);
                            if dark {
                                let dash = particle_color();
                                push_velocity_dash(
                                    out, geom, x, y, p.vx, vy, 0.08, thick, dash, dash, cov,
                                );
                            } else {
                                push_velocity_dash_over(
                                    out,
                                    geom,
                                    x,
                                    y,
                                    p.vx,
                                    vy,
                                    0.08,
                                    thick,
                                    Self::SPARKLE_PAPER_RIM,
                                    Self::paper_gold_alpha(cov),
                                );
                                push_paper_bead(
                                    out,
                                    geom,
                                    (x, y),
                                    thick * 1.2,
                                    (Self::SPARKLE_PAPER_RIM, Self::SPARKLE_PAPER_CORE),
                                    Self::paper_gold_alpha(cov),
                                );
                            }
                            continue;
                        }
                    }
                }
                _ => {
                    let (w, h) = if water {
                        let base = (ch * (0.055 + 0.065 * p.hue)).max(1.0);
                        let v_down = p.vy + p.gy * age;
                        let stretch = (v_down / ch).clamp(0.0, 1.6);
                        (
                            base.round() as i32,
                            (base * (1.0 + stretch)).round().max(1.0) as i32,
                        )
                    } else {
                        (sz, sz)
                    };
                    if !dark && water {
                        let r = (w.max(h) as f32 * 0.5).max(1.0) * STAR_ARM_INK;
                        push_paper_bead(
                            out,
                            geom,
                            (x, y),
                            r,
                            Self::LIGHT_WATER_DROP_INKS,
                            ink_alpha(cov),
                        );
                        continue;
                    }
                    if !dark && lawful {
                        let r = (w.max(h) as f32 * 0.5).max(1.0) * STAR_ARM_INK;
                        push_light_mote(halos, geom, (x, y), r, particle_color(), ink_alpha(cov));
                        continue;
                    }
                    if water {
                        let r = (w as f32 * 0.5 + 0.75).max(1.25);
                        let ry = (h as f32 * 0.5 + 0.75).max(r);
                        push_halo(
                            halos,
                            geom,
                            x,
                            y,
                            r * Self::WATER_DROP_SOFT,
                            ry * Self::WATER_DROP_SOFT,
                            premul_rgb(particle_color(), cov),
                        );
                        continue;
                    }
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

    /// The light ground a body under the letters solves its ink against ([`LightGround::of`]).
    #[inline]
    pub(super) const fn light_ground(&self, cfg: &GlowConfig) -> LightGround {
        let ground = LightGround::of(cfg);
        #[cfg(test)]
        let ground = LightGround {
            unsolved: self.light_body_unsolved,
            ..ground
        };
        ground
    }

    /// Whether a test asked for the light bodies' inks and ceilings unsolved.
    #[cfg(test)]
    pub(super) const fn light_body_unsolved(&self) -> bool {
        self.light_body_unsolved
    }
    #[cfg(not(test))]
    #[inline(always)]
    pub(super) const fn light_body_unsolved(&self) -> bool {
        false
    }

    /// Whether the dark bed is laid at each stop's full value rather than solved to the bar.
    #[cfg(test)]
    pub(super) const fn bed_unsolved(&self) -> bool {
        self.bed_unsolved
    }
    #[cfg(not(test))]
    #[inline(always)]
    pub(super) const fn bed_unsolved(&self) -> bool {
        false
    }

    /// Whether the frame keeps its dark bed but no over-ink account.
    #[cfg(test)]
    pub(super) const fn over_ink_idle(&self) -> bool {
        self.over_ink_idle
    }
    #[cfg(not(test))]
    #[inline(always)]
    pub(super) const fn over_ink_idle(&self) -> bool {
        false
    }

    /// Whether the dark streak skips its over-ink budget in tests.
    #[cfg(test)]
    pub(super) const fn streak_unbudgeted(&self) -> bool {
        self.streak_unbudgeted
    }
    #[cfg(not(test))]
    #[inline(always)]
    pub(super) const fn streak_unbudgeted(&self) -> bool {
        false
    }

    fn over_ink_ceiling(lift: [f32; 3]) -> f32 {
        let table = crate::color_math::relative_luminance_table();
        let unpack = |rgb: u32| [16u32, 8, 0].map(|sh| ((rgb >> sh) & 0xff) as f32);
        let ref_ink = unpack(Self::STREAK_REF_INK);
        let ref_ground = unpack(Self::STREAK_REF_GROUND);
        let lifted = |base: [f32; 3], cov: f32| {
            let ch = |c: usize| (base[c] + cov * lift[c]).min(255.0) as usize;
            crate::color_math::relative_luminance_rgb(table, ch(0), ch(1), ch(2))
        };
        let holds = |cov: f32| {
            let (ink, ground) = (lifted(ref_ink, cov), lifted(ref_ground, cov));
            ink.max(ground) + 0.05 >= Self::STREAK_INK_FLOOR * (ink.min(ground) + 0.05)
        };
        if holds(255.0) {
            return 255.0;
        }
        let (mut lo, mut hi) = (0.0f32, 255.0f32);
        for _ in 0..10 {
            let mid = 0.5 * (lo + hi);
            if holds(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The comet colour at path position `pos` (0 tail .. 1 head) for the style.
    pub(super) fn comet_color(&self, cfg: &GlowConfig, pos: f32) -> u32 {
        if matches!(cfg.style, GlowStyle::Sparkle) {
            return Self::sparkle_gold_at(cfg, pos);
        }
        let base = style_comet_color(cfg.style, cfg.color, cfg.accent, self.hue, pos);
        if matches!(cfg.style, GlowStyle::Water) && self.heat > 0.0 {
            return lerp_rgb(base, water_ramp(1.0), 0.5 * self.heat * pos);
        }
        if matches!(cfg.style, GlowStyle::Fire) {
            return fire_ramp((pos * (0.50 + 0.45 * self.fire_t())).min(0.92));
        }
        if matches!(cfg.style, GlowStyle::Phaser) && !cfg.dark_theme {
            return hsv2rgb(
                crate::color_math::light_spectrum_hue((self.hue + pos).fract()),
                0.95,
                1.0,
            );
        }
        base
    }

    /// Sparkle's gold on a dark ground at warm gold chroma.
    #[inline]
    pub(super) const fn sparkle_gold(cfg: &GlowConfig) -> u32 {
        crate::spectrum::spectrum_with_min_saturation(cfg.color & 0x00FF_FFFF, 0.72)
    }

    /// Sparkle's gold along its tail, `pos` 0 (tail) to 1 (head).
    #[inline]
    pub(super) fn sparkle_gold_at(cfg: &GlowConfig, pos: f32) -> u32 {
        Self::shade_gold_to_tail(Self::sparkle_gold(cfg), pos)
    }

    #[inline]
    fn shade_gold_to_tail(gold: u32, pos: f32) -> u32 {
        let (h, s, v) = crate::color_math::rgb2hsv(gold);
        let k = 1.0 - pos.clamp(0.0, 1.0);
        let k = k * k * (3.0 - 2.0 * k);
        crate::color_math::hsv2rgb(h - 4.0 * k, (s + 0.05 * k).min(1.0), v * (1.0 - 0.08 * k))
    }

    /// Sparkle's body gold on paper at `pos`, shaded toward the tail.
    #[inline]
    pub(crate) fn sparkle_paper_gold_at(cfg: &GlowConfig, pos: f32) -> u32 {
        let gold = if cfg.color & 0x00FF_FFFF == SPARKLE_DEFAULT_COLOR {
            Self::SPARKLE_PAPER_CORE
        } else {
            Self::sparkle_gold(cfg)
        };
        Self::shade_gold_to_tail(gold, pos)
    }

    /// A sparkle mark's opacity on paper for a dark coverage request.
    #[inline]
    pub(super) const fn paper_gold_alpha(cov: u8) -> u8 {
        (cov as f32 * 1.25).min(240.0) as u8
    }
}

/// Whether a cell's ground `ground` reads as the page `page` rather than a surface of its own.
pub(super) fn reads_as_page(ground: u32, page: u32) -> bool {
    let (g, p) = (ground & 0x00FF_FFFF, page & 0x00FF_FFFF);
    if g == p {
        return true;
    }
    let near = [16u32, 8, 0]
        .iter()
        .all(|&sh| ((g >> sh) & 0xff).abs_diff((p >> sh) & 0xff) <= SURFACE_CHANNEL);
    near && crate::color_math::contrast_ratio(g, p) < SURFACE_CONTRAST
}

const SURFACE_CONTRAST: f32 = 1.2;
const SURFACE_CHANNEL: u32 = 32;

/// The ASCII glyphs whose ink reaches under the baseline.
pub(super) const DESCENDER_GLYPHS: &str = "gjpqyQ,;_|()[]{}@$";

/// What stands in a burning row's cell for the dark flame's root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FlameGlyph {
    /// No glyph: the flame burns whole there.
    Blank,
    /// A letter, which may reach under the baseline.
    Letter { descends: bool },
}

impl FlameGlyph {
    /// Whether the band under the baseline is free of the cell's ink.
    #[inline]
    pub(super) const fn free_below(self) -> bool {
        !matches!(self, Self::Letter { descends: true })
    }
}

/// One dark tongue ([`CursorGlow::dark_tongues`]).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct DarkTongue {
    pub(super) u: f32,
    pub(super) half: f32,
    pub(super) size: f32,
    pub(super) e: f32,
    pub(super) kept: bool,
}

/// A burning row's dark tongues, allocation-free.
#[derive(Clone, Copy, Debug)]
pub(super) struct DarkTongues {
    buf: [DarkTongue; 48],
    n: usize,
}

impl Default for DarkTongues {
    fn default() -> Self {
        Self {
            buf: [DarkTongue::default(); 48],
            n: 0,
        }
    }
}

impl DarkTongues {
    #[inline]
    pub(super) fn as_slice(&self) -> &[DarkTongue] {
        &self.buf[..self.n]
    }

    #[inline]
    const fn push(&mut self, tongue: DarkTongue) {
        if self.n < self.buf.len() {
            self.buf[self.n] = tongue;
            self.n += 1;
        }
    }
}

const FLAME_CORE_STOPS: [(f32, u32); 4] = [
    (0.0, 0x00FF_F040),
    (0.30, 0x00FF_D820),
    (0.62, 0x00FF_A810),
    (1.0, 0x00F0_6010),
];

#[inline]
fn flame_core_ramp(rise: f32) -> u32 {
    let rise = rise.clamp(0.0, 1.0);
    let i = FLAME_CORE_STOPS
        .iter()
        .rposition(|s| s.0 <= rise)
        .unwrap_or(0)
        .min(FLAME_CORE_STOPS.len() - 2);
    let (a, b) = (FLAME_CORE_STOPS[i], FLAME_CORE_STOPS[i + 1]);
    lerp_rgb(a.1, b.1, (rise - a.0) / (b.0 - a.0))
}

#[inline]
fn flame_kernel(d: f32) -> f32 {
    let d = d.abs();
    if d >= 1.0 {
        0.0
    } else {
        0.5 * (1.0 + (std::f32::consts::PI * d).cos())
    }
}

#[inline]
fn flame_heat<'a>(cells: impl Iterator<Item = &'a (u16, u16, f32)>, u: f32) -> f32 {
    cells
        .map(|c| c.2 * flame_kernel(u - (f32::from(c.1) + 0.5)))
        .sum::<f32>()
        .min(1.0)
}

fn flame_dark_letter_cap(hot: u32, ink: u32, ground: u32, lift: [f32; 3], bar: f32) -> f32 {
    let table = crate::color_math::relative_luminance_table();
    let lift = lift.map(|l| l.max(0.0).ceil() as u32);
    let lifted = |base: u32, cov: u32| -> f32 {
        let ch = |sh: u32, i: usize| {
            let b = ((base >> sh) & 0xff) + lift[i];
            (b + (((hot >> sh) & 0xff) * cov + 127) / 255).min(255) as usize
        };
        crate::color_math::relative_luminance_rgb(table, ch(16, 0), ch(8, 1), ch(0, 2))
    };
    let ink_l = lifted(ink, 0);
    let reads = |cov: u32| {
        let g = lifted(ground, cov);
        (g < ink_l).then(|| (ink_l + 0.05) / (g + 0.05))
    };
    let Some(own) = reads(0) else {
        return 255.0;
    };
    let bar = bar.min(own);
    let (mut lo, mut hi) = (0u32, 256u32);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if reads(mid).is_some_and(|c| c >= bar) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo as f32
}

const FLAME_LEVEL_STEP: i32 = 64;

#[inline]
fn flame_level_of(idx: i32) -> usize {
    let level = (idx.max(0) + FLAME_LEVEL_STEP - 1) / FLAME_LEVEL_STEP;
    (level as usize).min(CursorGlow::FIRE_DARK_HELD_LEVELS - 1)
}

#[inline]
const fn flame_level_light(level: usize) -> u32 {
    if level + 1 >= CursorGlow::FIRE_DARK_HELD_LEVELS {
        return aterm_render::fire_field::FIRE_ADD_HOTTEST;
    }
    aterm_render::fire_field::fire_add_light(level as i32 * FLAME_LEVEL_STEP)
}

/// The dark flame field's ceiling as a share of its whole, at `rise` (0 at
/// the root, 1 at the tallest tongue's tip): whole over the lower third,
/// thinning to nothing at the tip, so the light dies upward into a point.
/// Thinned only to a quarter there, every slice of a tongue ended cut flat
/// a quarter lit, the tongues straight-sided and blunt as an equaliser's
/// bars (2026-10-22, `typing`, dark).
#[inline]
fn flame_tip_fade(rise: f32) -> f32 {
    const HOLD: f32 = 0.33;
    if rise <= HOLD {
        1.0
    } else {
        let k = ((rise - HOLD) / (1.0 - HOLD)).clamp(0.0, 1.0);
        (1.0 - k) * (1.0 - 0.25 * k)
    }
}

const FLAME_LIGHT_STOPS: [(f32, u32); 4] = [
    (0.0, 0x00FF_6F0A),
    (0.32, 0x00FF_8C14),
    (0.62, 0x00FF_A820),
    (1.0, 0x00FF_C848),
];

#[inline]
fn flame_light_paint_bounds() -> (u32, u32) {
    FLAME_LIGHT_STOPS
        .iter()
        .fold((0x00FF_FFFF, 0), |(lo, hi), &(_, rgb)| {
            (chan_min(lo, rgb), chan_max(hi, rgb))
        })
}

#[inline]
fn flame_light_ramp(rise: f32) -> u32 {
    let rise = rise.clamp(0.0, 1.0);
    let i = FLAME_LIGHT_STOPS
        .iter()
        .rposition(|s| s.0 <= rise)
        .unwrap_or(0)
        .min(FLAME_LIGHT_STOPS.len() - 2);
    let (a, b) = (FLAME_LIGHT_STOPS[i], FLAME_LIGHT_STOPS[i + 1]);
    lerp_rgb(a.1, b.1, (rise - a.0) / (b.0 - a.0))
}

fn fire_band_lifts(
    stack: &[(f32, f32, u32, f32, f32, usize)],
    color: u32,
    pos: f32,
    core: f32,
    ch: f32,
) -> BandLifts {
    let reaches: [f32; InkLedger::BANDS] =
        std::array::from_fn(|band| InkLedger::band_reach(ch, band));
    let mut lifts = [[0.0f32; 3]; InkLedger::BANDS];
    for layer in stack {
        let reach_max = 0.5 * layer.0 * core + 1.0;
        let one = layer_stack_lift(std::slice::from_ref(layer), color, pos);
        for (band, lift) in lifts.iter_mut().enumerate() {
            if reach_max >= reaches[band] {
                for (l, o) in lift.iter_mut().zip(one) {
                    *l += o;
                }
            }
        }
    }
    lifts
}

#[inline]
const fn streak_layers(style: GlowStyle) -> &'static [(f32, f32, u32, f32, f32, usize)] {
    match style {
        GlowStyle::Phaser => &PHASER_LAYERS,
        GlowStyle::Beam => &BEAM_LAYERS,
        GlowStyle::Laser => &LASER_LAYERS,
        GlowStyle::Sparkle => &SPARKLE_LAYERS,
        _ => &COMET_LAYERS,
    }
}

struct CometRuns<'a> {
    runs: &'a mut Vec<Vec<CometSample>>,
    run: &'a mut Vec<CometSample>,
    typing: &'a mut Vec<Vec<bool>>,
    run_typing: &'a mut Vec<bool>,
}

impl CometRuns<'_> {
    fn chain(
        &mut self,
        runs_len: &mut usize,
        prev: Option<(u16, u16)>,
        here: (u16, u16),
        sample: CometSample,
        typing: bool,
    ) {
        if breaks_comet_run(prev, here) {
            self.publish(runs_len);
        }
        self.run.push(sample);
        self.run_typing.push(typing);
    }

    fn publish(&mut self, runs_len: &mut usize) {
        if self.run.is_empty() {
            return;
        }
        let at = *runs_len;
        while self.typing.len() <= at {
            self.typing.push(Vec::new());
        }
        std::mem::swap(&mut self.typing[at], self.run_typing);
        self.run_typing.clear();
        publish_comet_run(self.runs, self.run, runs_len);
    }
}

fn breaks_comet_run(prev: Option<(u16, u16)>, here: (u16, u16)) -> bool {
    prev.is_some_and(|(pr, pc)| {
        (here.0 as i32 - pr as i32)
            .abs()
            .max((here.1 as i32 - pc as i32).abs())
            > 1
    })
}

#[inline]
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
