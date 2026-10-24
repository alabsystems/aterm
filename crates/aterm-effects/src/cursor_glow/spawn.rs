// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Spawns cursor-glow lights, particles, rings, bolts, and erase effects.

use super::*;

/// Floor of the momentum term, deliberately high.
pub(super) const ERASE_MOM_FLOOR: f32 = 0.55;
/// Ceiling on the weight term.
pub(super) const ERASE_WEIGHT_CAP: f32 = 3.4;
/// Sparkles at drive 1.0 ([`ERASE_MOM_FLOOR`], [`ERASE_POOF_CAP`]).
pub(super) const ERASE_POOF_STARS: f32 = 8.0;
/// Hard ceiling on one poof's sparkle count.
pub(super) const ERASE_POOF_CAP: usize = 18;
/// How wide, in cells, one poof's debris may spread whatever the erased span ([`erase_weight`]).
pub(super) const ERASE_POOF_SPREAD_CELLS: f32 = 9.0;
/// How much headroom above the erased row the debris needs before it is allowed to rise.
pub(super) const ERASE_POOF_LIFT_CELLS: f32 = 1.2;
/// The hero grain's `cov_scale` (its coverage multiplier in `emit_particles`).
pub(super) const ERASE_HERO_SCALE_MIN: f32 = 0.80;
pub(super) const ERASE_HERO_SCALE_SPAN: f32 = 0.09;
/// The poof's body grains' `cov_scale` (their coverage multiplier in `emit_particles`).
pub(super) const ERASE_ROUND_SCALE: f32 = 0.5;

/// The weight term ([`ERASE_WEIGHT_CAP`]).
#[inline]
pub(super) fn erase_weight(cells: u16) -> f32 {
    f32::from(cells.max(1)).sqrt().min(ERASE_WEIGHT_CAP)
}

/// The poof's energy ([`ERASE_MOM_FLOOR`], [`erase_weight`]).
#[inline]
pub(super) fn erase_poof_drive(mom: f32, cells: u16) -> f32 {
    (ERASE_MOM_FLOOR + (1.0 - ERASE_MOM_FLOOR) * mom.clamp(0.0, 1.0)) * erase_weight(cells)
}

impl CursorGlow {
    /// A non-typing move slams the flare, except under fire or on a navigation move.
    #[inline]
    pub(super) const fn retire_abandoned_light(&mut self, mv: &MoveCtx) {
        let &MoveCtx {
            typing,
            fire,
            navigation,
            ..
        } = mv;
        if !typing && !fire && !navigation {
            self.flare = 1.0;
        }
    }

    /// The aural twin of this spawn.
    pub(super) fn cue_move_sound(&mut self, mv: &MoveCtx) {
        let &MoveCtx {
            pr,
            pc,
            cr,
            cc,
            now,
            dist,
            typing,
            deletion,
            navigation,
            ..
        } = mv;
        if deletion {
            self.cue_sound(crate::trail_sound::SoundKind::Backspace, cc);
        } else if navigation {
            // While v2 owns the frame, the meteor gesture (or the nav tick) is
            // the one sound of a navigation move, minted by the engine; v1's
            // Sweep/Glide stay silent. Nested so a one-cell nav move cannot fall
            // through to the `typing` click and debit a key credit.
            if !self.v2.engaged() {
                let dir: i8 = if cc != pc {
                    if cc > pc { 1 } else { -1 }
                } else if cr < pr {
                    1
                } else {
                    -1
                };
                let kind = if dist >= Self::SWEEP_MIN_DIST {
                    crate::trail_sound::SoundKind::Sweep { dir }
                } else {
                    crate::trail_sound::SoundKind::Glide { dir }
                };
                self.cue_cursor(kind, cc, dir);
            }
        } else if typing {
            // One character, one click: a glyph the host already clicked at the
            // keypress ([`Self::cue_keystroke`]) spends that credit silently.
            if !self.take_keyed_click(now) {
                self.cue_sound(crate::trail_sound::SoundKind::Typed, cc);
            }
        } else {
            // A printable key whose echo moves the cursor more than one cell
            // classifies as a Jump, so its key credit is spent here too.
            if !self.take_keyed_click(now) {
                self.cue_sound(crate::trail_sound::SoundKind::Jump, cc);
            }
        }
    }

    /// Per-style birth brightness for a typing spark (1.0 on the jump path).
    #[inline]
    pub(super) const fn birth_boost(&self, mv: &MoveCtx) -> f32 {
        let &MoveCtx { cfg, typing, .. } = mv;
        if typing {
            match cfg.style {
                GlowStyle::Phaser => 1.0,
                GlowStyle::Fire => 0.28 + 0.92 * self.fire_t(),
                GlowStyle::Beam => 1.0,
                _ => self.typing_boost(),
            }
        } else {
            1.0
        }
    }

    /// Per-style spark lifetime for this move.
    pub(super) fn move_spark_life(&self, mv: &MoveCtx, gap: f32) -> f32 {
        let &MoveCtx { cfg, typing, .. } = mv;
        let full_life = cfg.duration.as_secs_f32().max(0.001);
        if typing {
            match cfg.style {
                GlowStyle::Phaser => {
                    if gap <= Self::PHASER_CHAIN_GAP_MAX {
                        (Self::PHASER_CHAIN_KEYS * gap + Self::CHAIN_MARGIN)
                            .min(Self::PHASER_CHAIN_LIFE_MAX)
                    } else {
                        (full_life * 2.0).clamp(0.40, 0.75)
                    }
                }
                GlowStyle::Water => {
                    let heat_life = (full_life * 0.42).clamp(0.06, 0.14) * (1.0 + 3.0 * self.heat);
                    if gap <= Self::CHAIN_GAP_MAX {
                        heat_life.max(
                            (Self::WATER_CHAIN_KEYS * gap + Self::CHAIN_MARGIN)
                                .min(Self::WATER_CHAIN_LIFE_MAX),
                        )
                    } else {
                        heat_life
                    }
                }
                GlowStyle::Fire => {
                    let heat_life =
                        (full_life * 0.38).clamp(0.10, 0.60) * (1.0 + 2.5 * self.fire_t());
                    if gap <= Self::CHAIN_GAP_MAX {
                        heat_life.max((gap + Self::CHAIN_MARGIN).min(Self::CHAIN_LIFE_MAX))
                    } else {
                        heat_life
                    }
                }
                GlowStyle::Beam => {
                    if gap <= Self::BEAM_CHAIN_GAP_MAX {
                        (Self::BEAM_CHAIN_KEYS * gap + Self::CHAIN_MARGIN)
                            .min(Self::BEAM_CHAIN_LIFE_MAX)
                    } else {
                        (full_life * 0.5).clamp(0.13, 0.40)
                    }
                }
                GlowStyle::Laser => {
                    let charge_life =
                        (full_life * 0.30).clamp(0.08, 0.40) * (1.0 + 1.8 * self.heat);
                    if gap <= Self::CHAIN_GAP_MAX {
                        charge_life.max((gap + Self::CHAIN_MARGIN).min(Self::CHAIN_LIFE_MAX))
                    } else {
                        charge_life
                    }
                }
                _ => {
                    let heat_life = (full_life * 0.38).clamp(0.05, 0.11) * (1.0 + 1.3 * self.heat);
                    if gap <= Self::CHAIN_GAP_MAX {
                        heat_life.max((gap + Self::CHAIN_MARGIN).min(Self::CHAIN_LIFE_MAX))
                    } else {
                        heat_life
                    }
                }
            }
        } else if matches!(cfg.style, GlowStyle::Phaser) {
            (full_life * 0.5).clamp(0.30, 0.90)
        } else {
            full_life
        }
    }

    /// Whether this spark is a resident, i.e. survives this frame's decay pass.
    #[inline]
    pub(super) fn spark_resident_at(now: Instant, s: &Spark) -> bool {
        now.saturating_duration_since(s.born).as_secs_f32() < s.life
    }

    /// Fire's one-line rule ([`Self::FLAME_SNUFF_S`], [`Self::SPARK_SNUFF_S`]).
    pub(super) fn snuff_off_line(
        &mut self,
        (cr, cc): (u16, u16),
        back_leap: bool,
        now: Instant,
        geom: Geom,
    ) {
        let cc_plus_1 = cc.saturating_add(1);
        let snuffed = |row: u16, col: u16| row != cr || (back_leap && col > cc_plus_1);
        for s in &mut self.sparks {
            if snuffed(s.row, s.col) {
                let age = now.saturating_duration_since(s.born).as_secs_f32();
                s.life = s.life.min(age + Self::FLAME_SNUFF_S);
            }
        }
        self.snuff_sparks_off_line(cr, back_leap, now, geom, Self::SPARK_SNUFF_S);
    }

    /// The flying half of [`Self::snuff_off_line`] ([`Self::SPARK_SNUFF_S`]).
    pub(super) fn snuff_sparks_off_line(
        &mut self,
        cr: u16,
        back_leap: bool,
        now: Instant,
        geom: Geom,
        within_s: f32,
    ) {
        if self.particles.is_empty() {
            return;
        }
        let (oy, chf) = (f32::from(geom.origin_y), geom.ch as f32);
        let cr_i32 = i32::from(cr);
        for p in &mut self.particles {
            if back_leap || ((p.y0 - oy) / chf).floor() as i32 != cr_i32 {
                let age = now.saturating_duration_since(p.born).as_secs_f32();
                p.life = p.life.min(age + within_s);
            }
        }
    }

    /// The per-move light.
    #[allow(
        clippy::too_many_arguments,
        reason = "the classified move + the shared per-move scalars spawn derives; a single internal call site"
    )]
    pub(super) fn spawn_move_light(
        &mut self,
        mv: &MoveCtx,
        gap: f32,
        boost: f32,
        spark_life: f32,
        hue_step: f32,
        fire_meteor: bool,
        reflow_licensed: bool,
    ) -> f32 {
        let &MoveCtx {
            pr,
            pc,
            cr,
            cc,
            now,
            cfg,
            geom,
            dr_abs,
            shape_wrap,
            pane_col0,
            pane_cols,
            echo_run,
            re_anchor,
            dist,
            typing,
            navigation,
            ..
        } = mv;
        let water = matches!(cfg.style, GlowStyle::Water);
        let mut hue_advances: f32 = 1.0;
        if fire_meteor {
            let back_leap = !typing && cc < pc && pr == cr;
            if (dr_abs >= 1 || back_leap) && !reflow_licensed {
                self.snuff_off_line((cr, cc), back_leap, now, geom);
            }
            let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
            let (x1, y1) = geom.cell_center(cr, cc);
            let (o_x, o_y) = geom.cell_center(pr, pc);
            let retarget = self.fire_meteors.iter_mut().rev().find(|m| {
                now.saturating_duration_since(m.born).as_secs_f32() <= Self::METEOR_RETARGET_S
                    && (m.x1 - o_x).abs() < cwf * 0.6
                    && (m.y1 - o_y).abs() < chf * 0.6
            });
            let life = (0.16 + 0.005 * dist).clamp(0.20, 0.45);
            if let Some(m) = retarget {
                m.x1 = x1;
                m.y1 = y1;
                m.life = m.life.max(life);
                m.born = now;
                m.arrived = false;
            } else {
                if self.fire_meteors.len() >= Self::METEOR_CAP {
                    self.fire_meteors.remove(0);
                }
                self.fire_meteors.push(Meteor {
                    x0: o_x,
                    y0: o_y,
                    x1,
                    y1,
                    born: now,
                    life,
                    mom: self.fire_t().max(0.55),
                    arrived: false,
                });
            }
        } else if navigation && (!water || dist < 2.0) {
        } else {
            let cap = Self::MAX_SPARKS;
            let max_len = if cfg
                .style
                .reads_knob(TrailKnob::Length, KnobContext::widest(false))
            {
                cfg.length.clamp(1, cap)
            } else {
                cap
            };
            self.path_scratch.clear();
            if shape_wrap {
                let pane_col1 = pane_col0.saturating_add(pane_cols);
                for c in (pc as i32)..(pane_col1 as i32) {
                    self.path_scratch.push((pr as i32, c));
                }
                for c in (pane_col0 as i32)..(cc as i32) {
                    self.path_scratch.push((cr as i32, c));
                }
            } else if re_anchor {
                self.path_scratch.push((cr as i32, cc as i32));
            } else {
                line_cells_tail(
                    &mut self.path_scratch,
                    (pr as i32, pc as i32),
                    (cr as i32, cc as i32),
                    max_len,
                    false,
                );
                if !echo_run
                    && !typing
                    && self.path_scratch.first() == Some(&(pr as i32, pc as i32))
                {
                    self.path_scratch.remove(0);
                }
            }
            let fire_back_leap = !typing && cc < pc && pr == cr;
            let phaser_wrap_follow = matches!(cfg.style, GlowStyle::Phaser) && typing;
            if matches!(cfg.style, GlowStyle::Fire | GlowStyle::Phaser)
                && (dr_abs >= 1 || fire_back_leap)
                && !phaser_wrap_follow
                && !reflow_licensed
            {
                if fire_back_leap || matches!(cfg.style, GlowStyle::Phaser) {
                    self.path_scratch.clear();
                } else {
                    self.path_scratch.retain(|&(r, _)| r == cr as i32);
                }
                self.snuff_off_line((cr, cc), fire_back_leap, now, geom);
            }
            // Water's leap off the line keeps to the caret's new row as fire's
            // does: an Enter's streak would run from the echo's end down across
            // the line the command printed, and its drops, left on that line's
            // word gaps, would read as punctuation (the trail gallery's
            // `enter-bottom`). The typed line's own wave fades as it would; a
            // navigation jump still spans its vector.
            if water && dr_abs >= 1 && !typing && !navigation && !reflow_licensed {
                self.path_scratch.retain(|&(r, _)| r == cr as i32);
            }
            if !self.path_scratch.is_empty() {
                let typing_run = echo_run || shape_wrap;
                if typing_run {
                    hue_advances = self.path_scratch.len() as f32;
                }
                let n = self.path_scratch.len() as f32;
                for i in 0..self.path_scratch.len() {
                    let (r, c) = self.path_scratch[i];
                    if r < 0 || c < 0 || r as usize >= geom.rows || c as usize >= geom.cols {
                        continue;
                    }
                    let pos = if typing_run {
                        1.0
                    } else {
                        (i as f32 + 1.0) / n
                    };
                    let born_cov = match cfg.style {
                        GlowStyle::Laser => ((205.0 + 50.0 * pos) * boost).min(255.0) as u8,
                        GlowStyle::Beam => ((165.0 + 70.0 * pos) * boost).min(240.0) as u8,
                        GlowStyle::Phaser => ((92.0 + 34.0 * pos) * boost).min(150.0) as u8,
                        GlowStyle::Fire => ((20.0 + 78.0 * pos) * boost).min(122.0) as u8,
                        _ => ((40.0 + 175.0 * pos) * boost).min(255.0) as u8,
                    };
                    self.sparks.push(Spark {
                        row: r as u16,
                        col: c as u16,
                        born_cov,
                        pos,
                        life: spark_life,
                        typing,
                        hue: if typing_run {
                            (self.hue + hue_step * i as f32 + 0.5).fract()
                        } else {
                            (self.hue + pos * 0.5).fract()
                        },
                        born: now,
                    });
                }
                if self.sparks.len() > cap {
                    let drop = self.sparks.len() - cap;
                    self.sparks.drain(0..drop);
                }
                if matches!(cfg.style, GlowStyle::Phaser | GlowStyle::Beam)
                    && typing
                    && gap <= Self::PHASER_CHAIN_GAP_MAX
                {
                    for s in self.sparks.iter_mut().rev().take(8) {
                        if s.typing && s.row == cr && s.col != cc && cc.abs_diff(s.col) <= 3 {
                            let age = now.saturating_duration_since(s.born).as_secs_f32();
                            s.life = s.life.max(age + spark_life);
                        }
                    }
                }
            }
        }
        hue_advances
    }

    /// Admit one lightning onset at `now`.
    pub(super) fn admit_bolt(&mut self, now: Instant, crackle: bool) -> bool {
        #[cfg(test)]
        if self.bolts_unlimited {
            return true;
        }
        let age = |t: Instant| now.saturating_duration_since(t).as_secs_f32();
        let n = Self::BOLT_FLASHES_PER_S;
        let oldest = self.bolt_onsets[self.bolt_onset_next];
        if oldest.is_some_and(|t| age(t) <= 1.0) {
            return false;
        }
        let last = self.bolt_onsets[(self.bolt_onset_next + n - 1) % n];
        if crackle && last.is_some_and(|t| age(t) < Self::CRACKLE_REFRACTORY_S) {
            return false;
        }
        self.bolt_onsets[self.bolt_onset_next] = Some(now);
        self.bolt_onset_next = (self.bolt_onset_next + 1) % n;
        true
    }

    /// Laser: a jump forges a strike along the jump vector.
    pub(super) fn spawn_lightning(&mut self, mv: &MoveCtx) {
        let &MoveCtx {
            pr,
            pc,
            cr,
            cc,
            now,
            cfg,
            geom,
            dist,
            navigation,
            ..
        } = mv;
        if matches!(cfg.style, GlowStyle::Laser) && !navigation {
            let chf = geom.ch as f32;
            if dist >= 2.0 {
                if self.admit_bolt(now, false) {
                    self.spawn_bolt(
                        geom.cell_center(pr, pc),
                        geom.cell_center(cr, cc),
                        chf,
                        true,
                        now,
                    );
                }
            } else if self.heat > 0.30
                && self.frand() < 0.18 + 0.55 * self.heat
                && self.admit_bolt(now, true)
            {
                let pick = self.frand();
                let a = self.frand() * std::f32::consts::TAU;
                let len = (1.6 + self.frand() * 2.8) * chf;
                let (rr, rc) = {
                    let mut cells = [(0u16, 0u16); Self::CRACKLE_TRAIL_CELLS];
                    let mut n = 0usize;
                    for s in self.sparks.iter().rev().take(Self::CRACKLE_TRAIL_CELLS) {
                        if s.typing && now.saturating_duration_since(s.born).as_secs_f32() < s.life
                        {
                            cells[n] = (s.row, s.col);
                            n += 1;
                        }
                    }
                    if n == 0 {
                        (cr, cc)
                    } else {
                        let i = ((pick * n as f32) as usize).min(n - 1);
                        cells[i]
                    }
                };
                let (x0, y0) = geom.cell_center(rr, rc);
                self.spawn_bolt(
                    (x0, y0),
                    (x0 + a.cos() * len, y0 + a.sin() * len),
                    chf,
                    false,
                    now,
                );
            }
        }
    }

    /// Landing ring on a real jump (fire meteors ring at arrival instead).
    pub(super) fn spawn_landing_ring(&mut self, mv: &MoveCtx, fire_meteor: bool) {
        let &MoveCtx {
            cr,
            cc,
            now,
            cfg,
            geom,
            dist,
            navigation,
            ..
        } = mv;
        let ring_on = cfg.ring
            && cfg
                .style
                .reads_knob(TrailKnob::Ring, KnobContext::widest(false));
        if ring_on && dist >= 2.0 && !fire_meteor && !navigation {
            let (cx, cy) = geom.cell_center(cr, cc);
            let scale = crate::rainbow_kitty::timing::impact(dist);
            self.ring = Some(Ring {
                cx,
                cy,
                born: now,
                life: classic_ring_life(0.18, scale),
                scale,
                caret: Some((cr, cc)),
            });
        }
    }

    /// The per-style particle burst for this move: jump-scaled showers,
    /// heat-scaled typing sheds; fire meteor moves defer to the strike, and
    /// navigation moves get none.
    pub(super) fn spawn_burst_particles(&mut self, mv: &MoveCtx, fire_meteor: bool) {
        let &MoveCtx {
            pr,
            pc,
            cr,
            cc,
            now,
            cfg,
            geom,
            dist,
            navigation,
            dr_abs,
            typing,
            ..
        } = mv;
        let leaves_line = dist >= 2.0 && (dr_abs >= 1 || (!typing && cc < pc && pr == cr));
        if leaves_line && cfg.style.has_particles() && !matches!(cfg.style, GlowStyle::Fire) {
            self.snuff_sparks_off_line(cr, true, now, geom, 0.0);
        }
        if cfg.style.has_particles() && !fire_meteor && !navigation {
            let (ox, oy) = geom.cell_center(cr, cc);
            let leap_from = {
                let (fx, fy) = geom.cell_center(pr, pc);
                let k = if leaves_line {
                    (Self::LEAP_BIRTH_SKIP_CELLS / dist.max(1.0)).min(0.5)
                } else {
                    0.0
                };
                (fx + (ox - fx) * k, fy + (oy - fy) * k)
            };
            let water = matches!(cfg.style, GlowStyle::Water);
            let fire = matches!(cfg.style, GlowStyle::Fire);
            let laser = matches!(cfg.style, GlowStyle::Laser);
            let beam = matches!(cfg.style, GlowStyle::Beam);
            let comet = matches!(cfg.style, GlowStyle::Comet);
            let blaze = if fire { self.fire_t() } else { self.blaze() };
            let burst = if dist >= 2.0 {
                if water {
                    (((6.0 + dist) * 1.5) as usize).min(26)
                } else if fire {
                    ((12.0 + dist * 1.8) as usize).min(44)
                } else if laser {
                    ((8.0 + dist * 1.3) as usize).min(30)
                } else if beam {
                    // A jump is the steady tube arriving and leaving, and
                    // nothing else: a leap's dust is the comet's. Fast typing
                    // coalesced into one move keeps the typing shed.
                    if typing {
                        1 + (self.heat * 4.0) as usize
                    } else {
                        0
                    }
                } else if comet {
                    ((5.0 + dist * 0.9) as usize).min(18)
                } else if matches!(cfg.style, GlowStyle::Sparkle) {
                    ((9.0 + dist * 1.3) as usize).min(32)
                } else {
                    (6.0 + dist).min(20.0) as usize
                }
            } else if water {
                1 + (self.heat * 6.0) as usize
            } else if fire {
                1 + (blaze * 13.0) as usize
            } else if laser {
                (self.heat * 5.0) as usize
            } else if beam {
                // Idle beam embers scale with residual heat.
                1 + (self.heat * 4.0) as usize
            } else if comet {
                1 + (self.heat * 4.0) as usize
            } else if matches!(cfg.style, GlowStyle::Sparkle) {
                2 + (self.heat * 6.0) as usize
            } else {
                1 + (self.heat * 3.0) as usize
            };
            let cell = geom.ch as f32;
            let hue0 = self.hue;
            let laser_cov_scale = if burst > 0 {
                (burst as f32).sqrt().recip()
            } else {
                1.0
            };
            let jl = if dist >= 2.0 { 0.5 } else { 1.0 };
            let pop_p = 0.25 * blaze;
            let smolder_p = 0.55 - 0.35 * blaze;
            let heat = self.heat;
            let bubble_p = if heat > 0.45 { 0.30 } else { 0.0 };
            let drip_p = 0.75 - 0.55 * heat;
            let comet_drift = {
                let (mvx, mvy) = (ox - leap_from.0, oy - leap_from.1);
                let ml = (mvx * mvx + mvy * mvy).sqrt();
                (ml > 0.5).then(|| (-mvx / ml, -mvy / ml))
            };
            for _ in 0..burst {
                let r0 = self.frand();
                let r1 = self.frand();
                let r2 = self.frand();
                let r3 = self.frand();
                let r4 = self.frand();
                let r5 = self.frand();
                let p = match cfg.style {
                    GlowStyle::Fire => {
                        let (bx, by) = if dist >= 2.0 {
                            let (fx, fy) = leap_from;
                            (fx + (ox - fx) * r0, fy + (oy - fy) * r0)
                        } else {
                            (ox + (r0 - 0.5) * geom.cw as f32, oy)
                        };
                        if r5 < pop_p {
                            Particle {
                                x0: bx,
                                y0: by,
                                vx: (r1 - 0.5) * 3.0 * cell,
                                vy: -(1.8 + r2 * 2.6) * cell,
                                gy: 1.0 * cell,
                                life: (0.22 + r3 * 0.24) * jl,
                                hue: 0.8 + r4 * 0.2,
                                cov_scale: 1.0,
                                born: now,
                            }
                        } else if r5 < pop_p + smolder_p {
                            Particle {
                                x0: bx + (r1 - 0.5) * 1.2 * geom.cw as f32,
                                y0: by + (r1 - 0.5) * 0.5 * cell,
                                vx: (r1 - 0.5) * 0.5 * cell,
                                vy: -(0.2 + r2 * 0.45) * cell,
                                gy: -0.3 * cell,
                                life: (0.9 + r3 * 0.8) * jl,
                                hue: r4 * 0.35,
                                cov_scale: 1.0,
                                born: now,
                            }
                        } else {
                            Particle {
                                x0: bx,
                                y0: by,
                                vx: (r1 - 0.5) * (0.8 + 1.1 * blaze) * cell,
                                vy: -(0.9 + r2 * (1.5 + 1.6 * blaze)) * cell,
                                gy: -0.6 * cell,
                                life: (0.45 + r3 * 0.55) * jl,
                                hue: 0.35 + r4 * 0.45,
                                cov_scale: 1.0,
                                born: now,
                            }
                        }
                    }
                    GlowStyle::Water => {
                        // Three droplet populations:
                        //   drip   - sags off the wake, then falls (dominant when
                        //            typing slowly);
                        //   spray  - kicked up and out, arcing back; energy
                        //            scales with heat;
                        //   bubble - buoyant, rising behind the head, only at
                        //            sustained speed.
                        if dist >= 2.0 {
                            if r5 < 0.4 {
                                Particle {
                                    x0: ox,
                                    y0: oy + 0.2 * cell,
                                    vx: (r1 - 0.5) * 3.2 * cell,
                                    vy: -(0.1 + r2 * 0.3) * cell,
                                    gy: 2.4 * cell,
                                    life: 0.25 + r3 * 0.25,
                                    hue: r4,
                                    cov_scale: 1.0,
                                    born: now,
                                }
                            } else {
                                Particle {
                                    x0: ox + (r0 - 0.5) * geom.cw as f32,
                                    y0: oy,
                                    vx: (r1 - 0.5) * 2.2 * cell,
                                    vy: -(0.8 + r2 * 1.6) * cell,
                                    gy: 3.0 * cell,
                                    life: 0.35 + r3 * 0.4,
                                    hue: r4,
                                    cov_scale: 1.0,
                                    born: now,
                                }
                            }
                        } else {
                            if r5 < bubble_p {
                                Particle {
                                    x0: ox - (0.4 + r0 * 1.6) * geom.cw as f32,
                                    y0: oy + (r1 - 0.5) * 0.5 * cell,
                                    vx: -(0.2 + r1 * 0.3) * cell,
                                    vy: -(0.15 + r2 * 0.35) * cell,
                                    gy: -0.8 * cell,
                                    life: 0.25 + r3 * 0.3,
                                    hue: r4,
                                    cov_scale: 1.0,
                                    born: now,
                                }
                            } else if r5 < bubble_p + drip_p {
                                Particle {
                                    x0: ox + (r0 - 0.5) * 0.8 * geom.cw as f32,
                                    y0: oy + 0.35 * cell,
                                    vx: (r1 - 0.5) * 0.15 * cell,
                                    vy: 0.05 * cell,
                                    gy: 4.2 * cell,
                                    life: 0.5 + r3 * 0.4,
                                    hue: r4,
                                    cov_scale: 1.0,
                                    born: now,
                                }
                            } else {
                                Particle {
                                    x0: ox + (r0 - 0.5) * geom.cw as f32,
                                    y0: oy,
                                    vx: (r1 - 0.5) * (1.2 + 1.6 * heat) * cell,
                                    vy: -(0.5 + r2 * (0.9 + 1.3 * heat)) * cell,
                                    gy: 3.0 * cell,
                                    life: 0.3 + r3 * 0.35,
                                    hue: r4,
                                    cov_scale: 1.0,
                                    born: now,
                                }
                            }
                        }
                    }
                    GlowStyle::Laser => {
                        let a = r0 * std::f32::consts::TAU;
                        let speed = (1.1 + r1 * 2.6) * cell;
                        Particle {
                            x0: ox,
                            y0: oy,
                            vx: a.cos() * speed * 1.4,
                            vy: a.sin() * speed * 0.5 - 0.25 * cell,
                            gy: 3.6 * cell,
                            life: 0.16 + r3 * 0.28,
                            hue: r4,
                            cov_scale: laser_cov_scale,
                            born: now,
                        }
                    }
                    GlowStyle::Beam => {
                        let (bx, by) = if dist >= 2.0 {
                            let (fx, fy) = leap_from;
                            (
                                fx + (ox - fx) * r0,
                                fy + (oy - fy) * r0 + (r1 - 0.5) * 0.8 * cell,
                            )
                        } else {
                            (
                                ox - (0.3 + r0 * 2.2) * geom.cw as f32,
                                oy + (r1 - 0.5) * 0.9 * cell,
                            )
                        };
                        Particle {
                            x0: bx,
                            y0: by,
                            vx: -(0.05 + r1 * 0.20) * cell,
                            vy: (r2 - 0.5) * 0.25 * cell,
                            gy: 0.0,
                            life: 0.55 + r3 * 0.85,
                            hue: r4,
                            cov_scale: 1.0,
                            born: now,
                        }
                    }
                    GlowStyle::Comet => {
                        let (fx, fy) = leap_from;
                        let (bx, by) = if dist >= 2.0 {
                            (fx + (ox - fx) * r0, fy + (oy - fy) * r0)
                        } else {
                            (ox + (r0 - 0.5) * geom.cw as f32, oy)
                        };
                        let (tx, ty) = comet_drift.unwrap_or_else(|| {
                            let a = r0 * std::f32::consts::TAU;
                            (a.cos(), a.sin())
                        });
                        let speed = (0.25 + r1 * 0.55) * cell;
                        Particle {
                            x0: bx,
                            y0: by,
                            vx: tx * speed + (r2 - 0.5) * 0.4 * cell,
                            vy: ty * speed + (r5 - 0.5) * 0.35 * cell,
                            gy: 0.25 * cell,
                            life: 0.45 + r3 * 0.6,
                            hue: r4,
                            cov_scale: 1.0,
                            born: now,
                        }
                    }
                    GlowStyle::Sparkle => {
                        let a = r0 * std::f32::consts::TAU;
                        let speed = (0.35 + r1 * 1.3) * cell;
                        let (bx, by) = if dist >= 2.0 {
                            let (fx, fy) = leap_from;
                            (
                                fx + (ox - fx) * r2,
                                fy + (oy - fy) * r2 + (r4 - 0.5) * 0.6 * cell,
                            )
                        } else {
                            (
                                ox + (r2 - 0.5) * geom.cw as f32,
                                oy + (r4 - 0.5) * 0.5 * cell,
                            )
                        };
                        Particle {
                            x0: bx,
                            y0: by,
                            vx: a.cos() * speed,
                            vy: a.sin() * speed - 0.15 * cell,
                            gy: 1.1 * cell,
                            life: 0.5 + r3 * 0.7,
                            hue: (hue0 + r5 * 0.3).fract(),
                            cov_scale: 1.0,
                            born: now,
                        }
                    }
                    _ => {
                        let a = r0 * std::f32::consts::TAU;
                        let speed = (0.4 + r1 * 1.6) * cell;
                        let (bx, by) = if dist >= 2.0 {
                            let (fx, fy) = leap_from;
                            (
                                fx + (ox - fx) * r2,
                                fy + (oy - fy) * r2 + (r4 - 0.5) * 0.6 * cell,
                            )
                        } else {
                            (
                                ox + (r2 - 0.5) * geom.cw as f32,
                                oy + (r4 - 0.5) * 0.5 * cell,
                            )
                        };
                        Particle {
                            x0: bx,
                            y0: by,
                            vx: a.cos() * speed,
                            vy: a.sin() * speed,
                            gy: 2.2 * cell,
                            life: 0.35 + r3 * 0.45,
                            hue: (hue0 + r5 * 0.3).fract(),
                            cov_scale: 1.0,
                            born: now,
                        }
                    }
                };
                self.particles.push(p);
            }
        }
    }

    /// A jump strike's main channel's shortest life, seconds.
    pub(super) const STRIKE_LIFE_S: f32 = 0.07;
    /// A strike's forks' shortest life, seconds.
    pub(super) const FORK_LIFE_S: f32 = 0.05;
    /// How much longer than its floor a strike or fork may live, seconds.
    pub(super) const STRIKE_LIFE_SPREAD_S: f32 = 0.03;

    /// Forge one lightning bolt from `from` to `to`: midpoint-displacement jagging.
    pub(super) fn spawn_bolt(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        cell: f32,
        big: bool,
        now: Instant,
    ) {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1.0 {
            return;
        }
        let rounds = if big { 4 } else { 2 };
        let (mut pts, mut next) = std::mem::take(&mut self.bolt_jag);
        pts.clear();
        pts.push(from);
        pts.push(to);
        let mut amp = (len * 0.16).clamp(cell * 0.35, cell * 2.6);
        for _ in 0..rounds {
            next.clear();
            next.reserve(pts.len() * 2 - 1);
            for w in pts.windows(2) {
                let (a, b) = (w[0], w[1]);
                let (sx, sy) = (b.0 - a.0, b.1 - a.1);
                let sl = (sx * sx + sy * sy).sqrt().max(1e-3);
                let kick = (self.frand() - 0.5) * 2.0 * amp;
                next.push(a);
                next.push((
                    (a.0 + b.0) * 0.5 - sy / sl * kick,
                    (a.1 + b.1) * 0.5 + sx / sl * kick,
                ));
            }
            next.push(*pts.last().expect("bolt polyline is never empty"));
            std::mem::swap(&mut pts, &mut next);
            amp *= 0.5;
        }
        if big {
            let forks = 2 + (self.frand() * 3.0) as usize;
            for _ in 0..forks {
                let i = (1 + (self.frand() * (pts.len() as f32 - 2.0)) as usize).min(pts.len() - 2);
                let root = pts[i];
                let prevp = pts[i - 1];
                let (tx, ty) = (root.0 - prevp.0, root.1 - prevp.1);
                let tl = (tx * tx + ty * ty).sqrt().max(1e-3);
                let bend = (self.frand() - 0.5) * 1.9;
                let (bc, bs) = (bend.cos(), bend.sin());
                let (ux, uy) = (tx / tl, ty / tl);
                let (bx, by) = (ux * bc - uy * bs, ux * bs + uy * bc);
                let blen = (len * (0.14 + self.frand() * 0.22)).max(cell);
                let tip = (root.0 + bx * blen, root.1 + by * blen);
                let kick = (self.frand() - 0.5) * blen * 0.5;
                let mid = (
                    (root.0 + tip.0) * 0.5 - (tip.1 - root.1) / blen * kick,
                    (root.1 + tip.1) * 0.5 + (tip.0 - root.0) / blen * kick,
                );
                let bolt = Bolt {
                    pts: vec![root, mid, tip],
                    born: now,
                    life: Self::FORK_LIFE_S + self.frand() * Self::STRIKE_LIFE_SPREAD_S,
                    branch: true,
                    crackle: false,
                };
                self.push_bolt(bolt);
            }
        }
        let life = if big {
            Self::STRIKE_LIFE_S + self.frand() * Self::STRIKE_LIFE_SPREAD_S
        } else {
            0.14 + self.frand() * 0.08
        };
        self.push_bolt(Bolt {
            pts: pts.clone(),
            born: now,
            life,
            branch: !big,
            crackle: !big,
        });
        self.bolt_jag = (pts, next);
    }

    /// Append a bolt, evicting the oldest once [`Self::MAX_BOLTS`] are live.
    #[inline]
    pub(super) fn push_bolt(&mut self, b: Bolt) {
        if self.bolts.len() >= Self::MAX_BOLTS {
            self.bolts.remove(0);
        }
        self.bolts.push(b);
    }

    /// The meteor's debris.
    pub(super) fn meteor_strike_fountain(&mut self, m: &Meteor, now: Instant, geom: Geom) {
        let cell = geom.ch as f32;
        let (dx, dy) = (m.x1 - m.x0, m.y1 - m.y0);
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        let dist_cells = len / geom.cw.max(1) as f32;
        let (ux, uy) = (dx / len, dy / len);
        let burst = ((6.0 + dist_cells * 1.0) as usize).min(24);
        for _ in 0..burst {
            if self.particles.len() >= Self::MAX_PARTICLES {
                break;
            }
            let r0 = self.frand();
            let r1 = self.frand();
            let r2 = self.frand();
            let r3 = self.frand();
            let r4 = self.frand();
            let t = 1.0 - Self::STRIKE_DEBRIS_REACH * (1.0 - r0.sqrt());
            let speed = (1.6 + 1.6 * m.mom) * cell * (0.4 + 0.6 * r2);
            self.particles.push(Particle {
                x0: m.x0 + dx * t,
                y0: m.y0 + dy * t,
                vx: ux * speed + (r1 - 0.5) * 0.5 * cell,
                vy: uy * speed - (0.1 + r2 * 0.25) * cell,
                gy: -0.15 * cell,
                life: 0.20 + r3 * 0.30,
                hue: 0.35 + r4 * 0.5,
                cov_scale: 1.0,
                born: now,
            });
        }
    }

    pub(super) fn poof_scan(&mut self, now: Instant, cfg: &GlowConfig, geom: Geom) {
        if self.kill_hint.is_none() && self.bs_poof_hint.is_none() {
            self.rotate_row_probe();
            return;
        }
        // Expire a stale hint: it keeps the animation timer armed
        // (`is_active`), so an unconsumed one must self-disarm.
        if self
            .kill_hint
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() > Self::KILL_HINT_FRESH)
        {
            self.kill_hint = None;
        }
        if self
            .bs_poof_hint
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() > Self::KILL_HINT_FRESH)
        {
            self.bs_poof_hint = None;
            self.bs_baseline = None;
        }
        if self.kill_hint.is_none() && self.bs_poof_hint.is_none() {
            self.rotate_row_probe();
            return;
        }
        let fresh_kill = true;
        let gap_ok = self
            .last_poof
            .is_none_or(|t| now.saturating_duration_since(t).as_secs_f32() >= Self::POOF_MIN_GAP);
        let bs_only = self.kill_hint.is_none();
        let voice = if bs_only {
            PoofVoice::Puff
        } else if self.kill_hint_word {
            PoofVoice::WordPoof
        } else {
            PoofVoice::Swoosh
        };
        let mut poofed = false;
        if fresh_kill
            && gap_ok
            && let (Some(prev), Some(cur)) = (self.row_prev_meta, self.row_cur_meta)
            && prev.trust == ProbeTrust::Full
            && cur.trust == ProbeTrust::Full
            && prev.row == cur.row
            && cur.fill < prev.fill
            && now.saturating_duration_since(prev.at).as_secs_f32() <= Self::POOF_PROBE_STALE
        {
            let cur_fill = cur.fill as usize;
            let prev_fill = prev.fill as usize;
            let p = self
                .row_prev
                .iter()
                .zip(self.row_cur.iter())
                .take(cur_fill)
                .take_while(|(a, b)| a == b)
                .count();
            let s = self.row_prev[..prev_fill.min(self.row_prev.len())]
                .iter()
                .rev()
                .zip(
                    self.row_cur[..cur_fill.min(self.row_cur.len())]
                        .iter()
                        .rev(),
                )
                .take(cur_fill)
                .take_while(|(a, b)| a == b)
                .count();
            if cur_fill > 0 && p + s >= cur_fill {
                let s_span = s.min(cur_fill - p.min(cur_fill));
                let c0 = p.min(prev.caret as usize).min(cur_fill) as u16;
                let c1 = (prev_fill - s_span).max(c0 as usize + 1) as u16;
                let survivor_holds_ink = s_span > 0
                    || self.row_cur[..(c0 as usize).min(self.row_cur.len())]
                        .iter()
                        .any(|&ch| ch != ' ');
                if survivor_holds_ink {
                    let n = (prev.fill - cur.fill).max(1);
                    self.spawn_poof(prev.row, c0, c1, n, now, cfg, geom, voice);
                    self.last_poof = Some(now);
                    self.kill_hint = None;
                    self.bs_poof_hint = None;
                    self.bs_baseline = None;
                    poofed = true;
                }
            }
        }
        let poof_hint_at = match (self.kill_hint, self.bs_poof_hint) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        let erasure_proven = self
            .row_prev_meta
            .zip(self.row_cur_meta)
            .is_some_and(|(prev, cur)| prev.row == cur.row && cur.fill < prev.fill);
        let erase_on_glass = self.poof_erase_witnessed();
        let bs_erased = erase_on_glass
            || matches!(
                (self.bs_baseline, self.row_cur_meta),
                (None, Some(cur)) if cur.caret > 0
            );
        if fresh_kill
            && gap_ok
            && !poofed
            && (!bs_only || erasure_proven || bs_erased)
            && (erase_on_glass
                || poof_hint_at.is_some_and(|t| {
                    now.saturating_duration_since(t).as_secs_f32() >= Self::POOF_FALLBACK_GRACE
                }))
            && let Some(cur) = self.row_cur_meta
            && cur.trust == ProbeTrust::Full
        {
            let c0 = cur.caret;
            self.spawn_poof(cur.row, c0, c0 + 3, 3, now, cfg, geom, voice);
            self.last_poof = Some(now);
            self.kill_hint = None;
            self.bs_poof_hint = None;
            self.bs_baseline = None;
        }
        self.rotate_row_probe();
    }

    /// Make this frame's row probe the one the emitters read ([`Self::probed_text_spans`]).
    #[inline]
    pub(super) fn rotate_row_probe(&mut self) {
        if self.row_cur_meta.is_some() {
            std::mem::swap(&mut self.row_prev, &mut self.row_cur);
            std::mem::swap(&mut self.row_above_prev, &mut self.row_above_cur);
            std::mem::swap(&mut self.row_below_prev, &mut self.row_below_cur);
            self.row_prev_meta = self.row_cur_meta.take();
            self.refresh_probed_text_spans();
        }
    }

    /// Emit one erase poof over the vanished span `[c0..c1)` of `row` ([`Self::note_kill`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "span + clock + config + geometry + cue ownership; kept explicit at the three poof_scan branches"
    )]
    pub(super) fn spawn_poof(
        &mut self,
        row: u16,
        c0: u16,
        c1: u16,
        n: u16,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        voice: PoofVoice,
    ) {
        match voice {
            PoofVoice::Swoosh => self.cue_sound(crate::trail_sound::SoundKind::Kill, c0),
            PoofVoice::WordPoof => self.cue_sound(crate::trail_sound::SoundKind::KillWord, c0),
            PoofVoice::Puff => self.cue_sound(crate::trail_sound::SoundKind::Poof, c0),
        }
        if self.v2.engaged() {
            let scope = match voice {
                PoofVoice::Swoosh => Some(rk::KillScope::Line),
                PoofVoice::WordPoof => Some(rk::KillScope::Word),
                PoofVoice::Puff => None,
            };
            if let Some(scope) = scope
                && let Some(key) = self.kill_hint
                && self.kill_reported_to_v2 != Some(key)
            {
                self.v2.on_event(rk::Event::Kill { cells: n, scope }, now);
                self.kill_reported_to_v2 = Some(key);
            }
        }
        let cell = geom.ch as f32;
        let cwf = geom.cw as f32;
        let span_x0 = geom.origin_x as f32 + c0 as f32 * cwf;
        let span_w = (c1.saturating_sub(c0)).max(1) as f32 * cwf;
        let oy = geom.origin_y as f32 + (row as f32 + 0.2) * cell;
        let nf = n as f32;
        if matches!(
            cfg.style,
            GlowStyle::RainbowKitty | GlowStyle::Sparkle | GlowStyle::Water | GlowStyle::Comet
        ) {
            self.spawn_poof_smoke(span_x0, span_w, oy, cell, cwf, n, now);
        }
        match cfg.style {
            GlowStyle::RainbowKitty | GlowStyle::Sparkle => {
                // Under v2 the erase's stars are stardust's; only the smoke above is v1's.
                if !self.v2.engaged() {
                    self.spawn_erase_sparkles(
                        span_x0,
                        span_w,
                        oy,
                        f32::from(geom.origin_y),
                        cell,
                        cwf,
                        n,
                        now,
                    );
                }
            }
            GlowStyle::Water | GlowStyle::Comet => {
                let comet = matches!(cfg.style, GlowStyle::Comet);
                let drops = (3 + (n as usize) / 8).min(5);
                for k in 0..drops {
                    if self.particles.len() >= Self::MAX_PARTICLES {
                        break;
                    }
                    let r0 = self.frand();
                    let r1 = self.frand();
                    let r2 = self.frand();
                    let r3 = self.frand();
                    let r4 = self.frand();
                    self.particles.push(Particle {
                        x0: span_x0 + ((k as f32 + 0.5) / drops as f32) * span_w + (r0 - 0.5) * cwf,
                        y0: oy,
                        vx: (r1 - 0.5) * 1.4 * cell,
                        vy: -(0.5 + r2 * 0.9) * cell,
                        gy: if comet { 0.25 * cell } else { 3.0 * cell },
                        life: if comet {
                            0.45 + r3 * 0.6
                        } else {
                            0.3 + r3 * 0.35
                        },
                        hue: r4,
                        cov_scale: 1.0,
                        born: now,
                    });
                }
            }
            GlowStyle::Fire => {
                let puffs = ((1.0 + nf.sqrt()) as usize).clamp(1, 5);
                let x_at = |k: usize| span_x0 + ((k as f32 + 0.5) / puffs as f32) * span_w;
                self.spawn_quench_steam(puffs, x_at, oy, cell, cwf, now);
            }
            _ => {
                self.spawn_poof_smoke(span_x0, span_w, oy, cell, cwf, n, now);
            }
        }
    }

    /// The erase sparkle poof (v1 rainbow kitty / sparkle).
    #[allow(
        clippy::too_many_arguments,
        reason = "span + geometry scalars + clock; the two internal call sites are the two erase detectors"
    )]
    pub(super) fn spawn_erase_sparkles(
        &mut self,
        span_x0: f32,
        span_w: f32,
        oy: f32,
        top: f32,
        cell: f32,
        cwf: f32,
        n: u16,
        now: Instant,
    ) {
        let mom = self.erase_mom.value(now);
        let drive = erase_poof_drive(mom, n);
        let lift = if oy - top < cell * ERASE_POOF_LIFT_CELLS {
            -1.0
        } else {
            1.0
        };
        let stars = ((ERASE_POOF_STARS * drive) as usize).clamp(4, ERASE_POOF_CAP);
        let speed = 0.55 + 0.85 * mom.clamp(0.0, 1.0);
        let heft = erase_weight(n) / ERASE_WEIGHT_CAP;
        let spread_w = span_w.min(cwf * ERASE_POOF_SPREAD_CELLS);
        let heft_y = 0.6 + 0.9 * heft;
        let gy = 0.9 * cell;
        let life_scale = 0.85 + 0.45 * heft;
        for k in 0..stars {
            if self.particles.len() >= Self::MAX_PARTICLES {
                break;
            }
            let r0 = self.frand();
            let r1 = self.frand();
            let r2 = self.frand();
            let r3 = self.frand();
            let r4 = self.frand();
            let f = (k as f32 + 0.5) / stars as f32;
            self.particles.push(Particle {
                x0: span_x0 + spread_w * f * f + (r0 - 0.5) * cwf,
                y0: oy + (r1 - 0.5) * cell * heft_y,
                vx: (0.35 + r2 * 1.25) * cell * speed,
                vy: -lift * (0.3 + r3 * 1.1) * cell * speed,
                gy,
                life: (0.42 + r4 * 0.55) * life_scale,
                hue: r0,
                cov_scale: ERASE_ROUND_SCALE,
                born: now,
            });
        }
        if self.particles.len() < Self::MAX_PARTICLES {
            let r0 = self.frand();
            let r1 = self.frand();
            self.particles.push(Particle {
                x0: span_x0 + span_w * 0.5,
                y0: oy - 0.1 * cell,
                vx: (r0 - 0.5) * 0.3 * cell,
                vy: -lift * (0.2 + r1 * 0.3) * cell,
                gy: 0.6 * cell,
                life: 0.30 + 0.26 * heft,
                hue: 0.02,
                cov_scale: ERASE_HERO_SCALE_MIN + ERASE_HERO_SCALE_SPAN * heft,
                born: now,
            });
        }
    }

    /// The neutral-grey smoke poof puffs.
    #[allow(
        clippy::too_many_arguments,
        reason = "span + geometry scalars; two internal call sites (spawn_poof)"
    )]
    pub(super) fn spawn_poof_smoke(
        &mut self,
        span_x0: f32,
        span_w: f32,
        oy: f32,
        cell: f32,
        cwf: f32,
        n: u16,
        now: Instant,
    ) {
        let puffs = ((1.0 + f32::from(n).sqrt() * 1.4) as usize).clamp(1, 8);
        let spread_w = span_w.min(cwf * ERASE_POOF_SPREAD_CELLS);
        for k in 0..puffs {
            if self.vapor.len() >= Self::MAX_VAPOR {
                break;
            }
            let (r0, r1, r2) = (self.frand(), self.frand(), self.frand());
            let f = (k as f32 + 0.5) / puffs as f32;
            self.vapor.push(Vapor {
                x0: span_x0 + spread_w * f * f + (r0 - 0.5) * cwf,
                y0: oy,
                vx: (r1 - 0.5) * 0.55 * cell,
                vy: -(0.55 + 0.45 * r2) * cell,
                gy: -0.10 * cell,
                life: 0.6 + 0.5 * r0,
                seed: r1,
                kind: VaporKind::Poof,
                born: now,
            });
        }
    }

    /// Quench steam ([`Self::update_typing_thermals`], [`Self::spawn_poof`]).
    pub(super) fn spawn_quench_steam(
        &mut self,
        puffs: usize,
        x_at: impl Fn(usize) -> f32,
        oy: f32,
        cell: f32,
        cw: f32,
        now: Instant,
    ) {
        for k in 0..puffs {
            if self.vapor.len() >= Self::MAX_VAPOR {
                break;
            }
            let (r0, r1, r2) = (self.frand(), self.frand(), self.frand());
            let hiss = k % 3 == 2;
            self.vapor.push(Vapor {
                x0: x_at(k) + (r0 - 0.5) * cw,
                y0: oy,
                vx: (r1 - 0.5) * (if hiss { 1.6 } else { 0.5 }) * cell,
                vy: -(0.5 + 0.5 * r2) * cell * (if hiss { 1.6 } else { 1.0 }),
                gy: 1.4 * cell,
                life: if hiss {
                    0.16 + 0.1 * r0
                } else {
                    0.5 + 0.4 * r0
                },
                seed: r1,
                kind: VaporKind::Steam,
                born: now,
            });
        }
    }
}

/// Lightning's flash law, checked at build time.
const _: () = assert!(
    CursorGlow::BOLT_FLASHES_PER_S as f32 <= TWINKLE_FLASH_BOUND_HZ
        && CursorGlow::CRACKLE_REFRACTORY_S > 1.0 / TWINKLE_FLASH_BOUND_HZ
        && CursorGlow::CRACKLE_REFRACTORY_S * (CursorGlow::BOLT_FLASHES_PER_S - 1) as f32 > 1.0,
    "lightning left the WCAG 2.3.1 flash envelope"
);

/// The laser streak's scintillation swells once per cycle.
const _: () = assert!(
    CursorGlow::LASER_RIPPLE_OMEGA / std::f32::consts::TAU <= TWINKLE_FLASH_BOUND_HZ,
    "the laser ripple flashes over the WCAG 2.3.1 general-flash bound"
);

/// A fire tongue's height flicker.
const _: () = assert!(
    (CursorGlow::TONGUE_FLICKER_RATE + CursorGlow::TONGUE_FLICKER_SPREAD)
        * (CursorGlow::FLAME_PHASE_SPEED + CursorGlow::FLAME_PHASE_HEAT_SPEED)
        <= TWINKLE_FLASH_BOUND_HZ,
    "a fire tongue flickers over the WCAG 2.3.1 general-flash bound"
);
