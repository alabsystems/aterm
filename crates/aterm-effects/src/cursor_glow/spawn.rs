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
pub(super) const fn erase_weight(cells: u16) -> f32 {
    let c = if cells > 1 { cells } else { 1 };
    crate::color_math::sqrt_f32(c as f32).min(ERASE_WEIGHT_CAP)
}

/// The poof's energy ([`ERASE_MOM_FLOOR`], [`erase_weight`]).
#[inline]
pub(super) const fn erase_poof_drive(mom: f32, cells: u16) -> f32 {
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
    #[inline]
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
    #[inline]
    pub(super) const fn move_spark_life(&self, mv: &MoveCtx, gap: f32) -> f32 {
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
    #[inline]
    pub(super) fn snuff_off_line(
        &mut self,
        (cr, cc): (u16, u16),
        back_leap: bool,
        now: Instant,
        geom: Geom,
    ) {
        let cc_plus_1 = cc.saturating_add(1);
        let mut si = 0;
        while si < self.sparks.len() {
            let s = &mut self.sparks[si];
            if s.row != cr || (back_leap && s.col > cc_plus_1) {
                let age = now.saturating_duration_since(s.born).as_secs_f32();
                s.life = s.life.min(age + Self::FLAME_SNUFF_S);
            }
            si += 1;
        }
        self.snuff_sparks_off_line(cr, back_leap, now, geom, Self::SPARK_SNUFF_S);
    }

    /// The flying half of [`Self::snuff_off_line`] ([`Self::SPARK_SNUFF_S`]).
    #[inline]
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
        let (oy, inv_ch) = (f32::from(geom.origin_y), 1.0 / (geom.ch as f32));
        let cr_i32 = i32::from(cr);
        let mut pi = 0;
        while pi < self.particles.len() {
            let p = &mut self.particles[pi];
            if back_leap || ((p.y0 - oy) * inv_ch).floor() as i32 != cr_i32 {
                let age = now.saturating_duration_since(p.born).as_secs_f32();
                p.life = p.life.min(age + within_s);
            }
            pi += 1;
        }
    }

    /// The per-move light.
    #[allow(
        clippy::too_many_arguments,
        reason = "the classified move + the shared per-move scalars spawn derives; a single internal call site"
    )]
    #[inline]
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
            let life = (0.16 + 0.005 * dist).clamp(0.20, 0.45);
            let mut retarget_idx = None;
            let mut mi = self.fire_meteors.len();
            while mi > 0 {
                mi -= 1;
                let m = &self.fire_meteors[mi];
                if now.saturating_duration_since(m.born) <= METEOR_RETARGET_DUR
                    && (m.x1 - o_x).abs() < cwf * 0.6
                    && (m.y1 - o_y).abs() < chf * 0.6
                {
                    retarget_idx = Some(mi);
                    break;
                }
            }
            if let Some(mi) = retarget_idx {
                let m = &mut self.fire_meteors[mi];
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
                let pane_col1 = pane_col0.saturating_add(pane_cols) as i32;
                let mut c = pc as i32;
                while c < pane_col1 {
                    self.path_scratch.push((pr as i32, c));
                    c += 1;
                }
                let mut c = pane_col0 as i32;
                let cc_i32 = cc as i32;
                while c < cc_i32 {
                    self.path_scratch.push((cr as i32, c));
                    c += 1;
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
                let inv_n = 1.0 / (self.path_scratch.len() as f32);
                let mut i = 0;
                while i < self.path_scratch.len() {
                    let (r, c) = self.path_scratch[i];
                    if r >= 0 && c >= 0 && (r as usize) < geom.rows && (c as usize) < geom.cols {
                        let pos = if typing_run {
                            1.0
                        } else {
                            (i as f32 + 1.0) * inv_n
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
                    i += 1;
                }
                if self.sparks.len() > cap {
                    let drop = self.sparks.len() - cap;
                    self.sparks.drain(0..drop);
                }
                if matches!(cfg.style, GlowStyle::Phaser | GlowStyle::Beam)
                    && typing
                    && gap <= Self::PHASER_CHAIN_GAP_MAX
                {
                    let len = self.sparks.len();
                    let stop = len.saturating_sub(8);
                    let mut si = len;
                    while si > stop {
                        si -= 1;
                        let s = &mut self.sparks[si];
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
    #[inline]
    pub(super) fn admit_bolt(&mut self, now: Instant, crackle: bool) -> bool {
        #[cfg(test)]
        if self.bolts_unlimited {
            return true;
        }
        const ONE_SEC: Duration = Duration::from_secs(1);
        let n = Self::BOLT_FLASHES_PER_S;
        let oldest = self.bolt_onsets[self.bolt_onset_next];
        if let Some(t) = oldest
            && now.saturating_duration_since(t) <= ONE_SEC
        {
            return false;
        }
        let last = self.bolt_onsets[(self.bolt_onset_next + n - 1) % n];
        if crackle
            && let Some(t) = last
            && now.saturating_duration_since(t) < CRACKLE_REFRACTORY_DUR
        {
            return false;
        }
        self.bolt_onsets[self.bolt_onset_next] = Some(now);
        self.bolt_onset_next = (self.bolt_onset_next + 1) % n;
        true
    }

    /// Laser: a jump forges a strike along the jump vector.
    #[inline]
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
                    self.keep_strike_in_grid(geom, cfg.theme_bg, (pr, cr), now);
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
                    let spark_len = self.sparks.len();
                    let stop = spark_len.saturating_sub(Self::CRACKLE_TRAIL_CELLS);
                    let mut si = spark_len;
                    while si > stop {
                        si -= 1;
                        let s = &self.sparks[si];
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
                let (sin_a, cos_a) = crate::supernova::sin_cos_f32(a);
                #[cfg(test)]
                let rooted = self.crackle_rooted_in_letters;
                #[cfg(not(test))]
                let rooted = false;
                // A CRACKLE SPRINGS FROM ITS ROW'S EDGE INTO THE AIR. Rooted
                // at a typed cell's centre at any angle, the guard hid its
                // root among the letters and what showed was a lone violet
                // dash under or over the line, apart from the bolt and the
                // letters (Solarized's and the dark page's `burst.t0`, a
                // smooth arc across the `f` of `flies` on Default Light;
                // looks10 round 2's L8). It leaves its typed cell by the
                // edge of the row it is aimed at, into a blank row, steeply
                // enough to clear the row's letters; with no blank row on
                // either side it is not forged.
                let aim = if rooted {
                    Some(((x0, y0), (x0 + cos_a * len, y0 + sin_a * len)))
                } else {
                    let text = self.text_view();
                    let blank = |r: i32| {
                        r >= 0 && r < geom.rows as i32 && text.knows_row(r) && text.row_is_blank(r)
                    };
                    let row = i32::from(rr);
                    let first_up = sin_a < 0.0;
                    let up = if blank(row + if first_up { -1 } else { 1 }) {
                        Some(first_up)
                    } else if blank(row + if first_up { 1 } else { -1 }) {
                        Some(!first_up)
                    } else {
                        None
                    };
                    up.map(|up| {
                        let way = if up { -1.0 } else { 1.0 };
                        let edge = y0 + way * 0.5 * chf;
                        let rise = sin_a.abs().max(Self::CRACKLE_LEAVE_SIN);
                        let run = (1.0 - rise * rise).max(0.0).sqrt()
                            * if cos_a < 0.0 { -1.0 } else { 1.0 };
                        ((x0, edge), (x0 + run * len, edge + way * rise * len))
                    })
                };
                let Some((from, to)) = aim else {
                    return;
                };
                self.spawn_bolt(from, to, chf, false, now);
                // Its kinks swing about its chord; one swung back over the
                // row's edge is folded back into the air, so no part of it
                // lies on the letters it sprang from.
                if !rooted
                    && from.1 != y0
                    && let Some(b) = self.bolts.last_mut()
                    && b.crackle
                {
                    let (edge, down) = (from.1, from.1 > y0);
                    for p in b.pts.as_mut_slice() {
                        if (down && p.1 < edge) || (!down && p.1 > edge) {
                            p.1 = 2.0 * edge - p.1;
                        }
                    }
                }
                #[cfg(test)]
                let kept = !self.crackle_unkept;
                #[cfg(not(test))]
                let kept = true;
                if kept {
                    self.keep_crackle_off_bars(geom, cfg.theme_bg, cr, y0);
                }
            }
        }
    }

    /// Landing ring on a real jump (fire meteors ring at arrival instead).
    #[inline]
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
    #[inline]
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
                // Two motes a key from cold: one, most of a slow line's
                // dust lost to the leading's seats, left the tail bare.
                2 + (self.heat * 5.0) as usize
            } else if matches!(cfg.style, GlowStyle::Sparkle) {
                2 + (self.heat * 6.0) as usize
            } else {
                1 + (self.heat * 3.0) as usize
            };
            let cell = geom.ch as f32;
            let cwf = geom.cw as f32;
            let hue0 = self.hue;
            let laser_cov_scale = if laser && burst > 0 {
                1.0 / crate::color_math::sqrt_f32(burst as f32)
            } else {
                1.0
            };
            let jl = if dist >= 2.0 { 0.5 } else { 1.0 };
            let pop_p = 0.25 * blaze;
            let smolder_p = 0.55 - 0.35 * blaze;
            let heat = self.heat;
            let bubble_p = if heat > 0.45 { 0.30 } else { 0.0 };
            let drip_p = 0.75 - 0.55 * heat;
            #[cfg(test)]
            let comet_regimented = self.comet_dust_regimented;
            #[cfg(not(test))]
            let comet_regimented = false;
            let comet_drift = if comet {
                let (mvx, mvy) = (ox - leap_from.0, oy - leap_from.1);
                let ml = crate::color_math::hypot_f32(mvx, mvy);
                if ml > 0.5 {
                    let inv_ml = 1.0 / ml;
                    Some((-mvx * inv_ml, -mvy * inv_ml))
                } else {
                    None
                }
            } else {
                None
            };
            let mut bi = 0;
            while bi < burst {
                bi += 1;
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
                            (ox + (r0 - 0.5) * cwf, oy)
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
                                latch: ParticleLatch::FRESH,
                                edge_cut: false,
                            }
                        } else if r5 < pop_p + smolder_p {
                            Particle {
                                x0: bx + (r1 - 0.5) * 1.2 * cwf,
                                y0: by + (r1 - 0.5) * 0.5 * cell,
                                vx: (r1 - 0.5) * 0.5 * cell,
                                vy: -(0.2 + r2 * 0.45) * cell,
                                gy: -0.3 * cell,
                                life: (0.9 + r3 * 0.8) * jl,
                                hue: r4 * 0.35,
                                cov_scale: 1.0,
                                born: now,
                                latch: ParticleLatch::FRESH,
                                edge_cut: false,
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
                                latch: ParticleLatch::FRESH,
                                edge_cut: false,
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
                                    latch: ParticleLatch::FRESH,
                                    edge_cut: false,
                                }
                            } else {
                                Particle {
                                    x0: ox + (r0 - 0.5) * cwf,
                                    y0: oy,
                                    vx: (r1 - 0.5) * 2.2 * cell,
                                    vy: -(0.8 + r2 * 1.6) * cell,
                                    gy: 3.0 * cell,
                                    life: 0.35 + r3 * 0.4,
                                    hue: r4,
                                    cov_scale: 1.0,
                                    born: now,
                                    latch: ParticleLatch::FRESH,
                                    edge_cut: false,
                                }
                            }
                        } else {
                            if r5 < bubble_p {
                                Particle {
                                    x0: ox - (0.4 + r0 * 1.6) * cwf,
                                    y0: oy + (r1 - 0.5) * 0.5 * cell,
                                    vx: -(0.2 + r1 * 0.3) * cell,
                                    vy: -(0.15 + r2 * 0.35) * cell,
                                    gy: -0.8 * cell,
                                    life: 0.25 + r3 * 0.3,
                                    hue: r4,
                                    cov_scale: 1.0,
                                    born: now,
                                    latch: ParticleLatch::FRESH,
                                    edge_cut: false,
                                }
                            } else if r5 < bubble_p + drip_p {
                                Particle {
                                    x0: ox + (r0 - 0.5) * 0.8 * cwf,
                                    y0: oy + 0.35 * cell,
                                    vx: (r1 - 0.5) * 0.15 * cell,
                                    vy: 0.05 * cell,
                                    gy: 4.2 * cell,
                                    life: 0.5 + r3 * 0.4,
                                    hue: r4,
                                    cov_scale: 1.0,
                                    born: now,
                                    latch: ParticleLatch::FRESH,
                                    edge_cut: false,
                                }
                            } else {
                                Particle {
                                    x0: ox + (r0 - 0.5) * cwf,
                                    y0: oy,
                                    vx: (r1 - 0.5) * (1.2 + 1.6 * heat) * cell,
                                    vy: -(0.5 + r2 * (0.9 + 1.3 * heat)) * cell,
                                    gy: 3.0 * cell,
                                    life: 0.3 + r3 * 0.35,
                                    hue: r4,
                                    cov_scale: 1.0,
                                    born: now,
                                    latch: ParticleLatch::FRESH,
                                    edge_cut: false,
                                }
                            }
                        }
                    }
                    GlowStyle::Laser => {
                        let (sin_a, cos_a) =
                            crate::supernova::sin_cos_f32(r0 * std::f32::consts::TAU);
                        let speed = (1.1 + r1 * 2.6) * cell;
                        Particle {
                            x0: ox,
                            y0: oy,
                            vx: cos_a * speed * 1.4,
                            vy: sin_a * speed * 0.5 - 0.25 * cell,
                            gy: 3.6 * cell,
                            life: 0.16 + r3 * 0.28,
                            hue: r4,
                            cov_scale: laser_cov_scale,
                            born: now,
                            latch: ParticleLatch::FRESH,
                            edge_cut: false,
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
                            (ox - (0.3 + r0 * 2.2) * cwf, oy + (r1 - 0.5) * 0.9 * cell)
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
                            latch: ParticleLatch::FRESH,
                            edge_cut: false,
                        }
                    }
                    GlowStyle::Comet => {
                        let (fx, fy) = leap_from;
                        // A typed key's dust is shed behind the nucleus, from
                        // the caret's trailing edge back a cell: born about the
                        // caret's centre, half of it stood ahead of the head
                        // (looks9 Q2).
                        let (bx, by) = if dist >= 2.0 {
                            (fx + (ox - fx) * r0, fy + (oy - fy) * r0)
                        } else {
                            match comet_drift.filter(|_| !comet_regimented) {
                                Some((tx, ty)) => {
                                    let back = (0.3 + 0.8 * r0) * cwf;
                                    (ox + tx * back, oy + ty * back)
                                }
                                None => (ox + (r0 - 0.5) * cwf, oy),
                            }
                        };
                        let (tx, ty) = match comet_drift {
                            Some(d) => d,
                            None => {
                                let (sin_a, cos_a) =
                                    crate::supernova::sin_cos_f32(r0 * std::f32::consts::TAU);
                                (cos_a, sin_a)
                            }
                        };
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
                            latch: ParticleLatch::FRESH,
                            edge_cut: false,
                        }
                    }
                    GlowStyle::Sparkle => {
                        let (sin_a, cos_a) =
                            crate::supernova::sin_cos_f32(r0 * std::f32::consts::TAU);
                        let speed = (0.35 + r1 * 1.3) * cell;
                        let (bx, by) = if dist >= 2.0 {
                            let (fx, fy) = leap_from;
                            (
                                fx + (ox - fx) * r2,
                                fy + (oy - fy) * r2 + (r4 - 0.5) * 0.6 * cell,
                            )
                        } else {
                            (ox + (r2 - 0.5) * cwf, oy + (r4 - 0.5) * 0.5 * cell)
                        };
                        Particle {
                            x0: bx,
                            y0: by,
                            vx: cos_a * speed,
                            vy: sin_a * speed - 0.15 * cell,
                            gy: 1.1 * cell,
                            life: 0.5 + r3 * 0.7,
                            hue: (hue0 + r5 * 0.3).fract(),
                            cov_scale: 1.0,
                            born: now,
                            latch: ParticleLatch::FRESH,
                            edge_cut: false,
                        }
                    }
                    _ => {
                        let (sin_a, cos_a) =
                            crate::supernova::sin_cos_f32(r0 * std::f32::consts::TAU);
                        let speed = (0.4 + r1 * 1.6) * cell;
                        let (bx, by) = if dist >= 2.0 {
                            let (fx, fy) = leap_from;
                            (
                                fx + (ox - fx) * r2,
                                fy + (oy - fy) * r2 + (r4 - 0.5) * 0.6 * cell,
                            )
                        } else {
                            (ox + (r2 - 0.5) * cwf, oy + (r4 - 0.5) * 0.5 * cell)
                        };
                        Particle {
                            x0: bx,
                            y0: by,
                            vx: cos_a * speed,
                            vy: sin_a * speed,
                            gy: 2.2 * cell,
                            life: 0.35 + r3 * 0.45,
                            hue: (hue0 + r5 * 0.3).fract(),
                            cov_scale: 1.0,
                            born: now,
                            latch: ParticleLatch::FRESH,
                            edge_cut: false,
                        }
                    }
                };
                self.particles.push(p);
            }
        }
    }

    /// The least sine of the angle a crackle leaves its row's edge at
    /// ([`Self::spawn_lightning`]): steep enough that its kinks keep off the
    /// letters it springs from.
    pub(super) const CRACKLE_LEAVE_SIN: f32 = 0.6;

    /// The longest straight span of a crackle's channel, in cells
    /// ([`Self::spawn_bolt`]).
    pub(super) const CRACKLE_SPAN_CELLS: f32 = 0.5;
    /// The most a crackle's knot is kicked off its span, as a share of the
    /// span ([`Self::spawn_bolt`]): enough to kink it as lightning does, and
    /// little enough that its longest span stays within three quarters of a
    /// cell ([`Self::CRACKLE_SPAN_CELLS`] grows by at most `sqrt(1 + 4k²)` a
    /// round).
    pub(super) const CRACKLE_KINK: f32 = 0.27;
    /// The least share of [`Self::CRACKLE_KINK`] a crackle's knot is kicked
    /// by, so every knot is a kink, not a bend.
    pub(super) const CRACKLE_KINK_FLOOR: f32 = 0.65;
    /// A jump strike's main channel's shortest life, seconds.
    pub(super) const STRIKE_LIFE_S: f32 = 0.07;
    /// A strike's forks' shortest life, seconds.
    pub(super) const FORK_LIFE_S: f32 = 0.05;
    /// How much longer than its floor a strike or fork may live, seconds.
    pub(super) const STRIKE_LIFE_SPREAD_S: f32 = 0.03;

    /// Forge one lightning bolt from `from` to `to`: midpoint-displacement jagging.
    #[inline]
    pub(super) fn spawn_bolt(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        cell: f32,
        big: bool,
        now: Instant,
    ) {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let len = crate::color_math::hypot_f32(dx, dy);
        if len < 1.0 {
            return;
        }
        // A crackle kinks as finely as a strike does, a span to every
        // [`Self::CRACKLE_SPAN_CELLS`] of a cell or less: forged in two rounds
        // whatever its length, a long one was four straight spans, a smooth
        // bent wire in the air above the line (Solarized Dark's `burst.t0`,
        // looks9's final look).
        #[cfg(test)]
        let two_rounds = self.crackle_two_rounds;
        #[cfg(not(test))]
        let two_rounds = false;
        let rounds = if big {
            4
        } else if two_rounds {
            2
        } else {
            let spans = len / (cell * Self::CRACKLE_SPAN_CELLS);
            if spans <= 4.0 {
                2
            } else if spans <= 8.0 {
                3
            } else {
                4
            }
        };
        // A CRACKLE ZIGZAGS. Its knots kicked by any amount either way, the
        // kick halving each round as a strike's does, a short crackle came
        // out a smooth arc, every knot bent by a few degrees: a bent-wire
        // scratch in the air over the line, not lightning (Default Light's
        // `burst.t0`, looks10 round 2's L8). Each of its knots is kicked off
        // its own span by a share of it, the other way from the knot before,
        // never less than [`Self::CRACKLE_KINK_FLOOR`] of the most, so every
        // knot is a kink. A strike keeps its halving kicks: its forks and its
        // length kink it.
        #[cfg(test)]
        let smooth = self.crackle_smooth_kicks;
        #[cfg(not(test))]
        let smooth = false;
        let zigzag = !big && !smooth;
        let mut side = if zigzag && self.frand() < 0.5 {
            -1.0
        } else {
            1.0
        };
        let (mut pts, mut next) = (BoltPts::default(), BoltPts::default());
        pts.push(from);
        pts.push(to);
        let mut amp = (len * 0.16).clamp(cell * 0.35, cell * 2.6);
        let mut round = 0;
        while round < rounds {
            round += 1;
            next.len = 0;
            let pts_slice = pts.as_slice();
            let mut wi = 0;
            while wi + 1 < pts_slice.len() {
                let (a, b) = (pts_slice[wi], pts_slice[wi + 1]);
                let (sx, sy) = (b.0 - a.0, b.1 - a.1);
                let kick_sl = if zigzag {
                    let floor = Self::CRACKLE_KINK_FLOOR;
                    let k = side * Self::CRACKLE_KINK * (floor + (1.0 - floor) * self.frand());
                    side = -side;
                    k
                } else {
                    let inv_sl = 1.0 / crate::color_math::hypot_f32(sx, sy).max(1e-3);
                    (self.frand() - 0.5) * 2.0 * amp * inv_sl
                };
                next.push(a);
                next.push((
                    (a.0 + b.0) * 0.5 - sy * kick_sl,
                    (a.1 + b.1) * 0.5 + sx * kick_sl,
                ));
                wi += 1;
            }
            next.push(pts_slice[pts_slice.len() - 1]);
            std::mem::swap(&mut pts, &mut next);
            amp *= 0.5;
        }
        if big {
            let forks = 2 + (self.frand() * 3.0) as usize;
            let mut fi = 0;
            while fi < forks {
                fi += 1;
                let i = (1 + (self.frand() * (pts.len() as f32 - 2.0)) as usize).min(pts.len() - 2);
                let root = pts[i];
                let prevp = pts[i - 1];
                let (tx, ty) = (root.0 - prevp.0, root.1 - prevp.1);
                let inv_tl = 1.0 / crate::color_math::hypot_f32(tx, ty).max(1e-3);
                let bend = (self.frand() - 0.5) * 1.9;
                let (bs, bc) = crate::supernova::sin_cos_f32(bend);
                let (ux, uy) = (tx * inv_tl, ty * inv_tl);
                let (bx, by) = (ux * bc - uy * bs, ux * bs + uy * bc);
                let blen = (len * (0.14 + self.frand() * 0.22)).max(cell);
                let tip = (root.0 + bx * blen, root.1 + by * blen);
                let kick = (self.frand() - 0.5) * blen * 0.5;
                let mid = (
                    (root.0 + tip.0) * 0.5 - (tip.1 - root.1) / blen * kick,
                    (root.1 + tip.1) * 0.5 + (tip.0 - root.0) / blen * kick,
                );
                let bolt = Bolt {
                    pts: BoltPts::from_3(root, mid, tip),
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
            pts,
            born: now,
            life,
            branch: !big,
            crackle: !big,
        });
    }

    /// Whether the bolt along `pts` (`branch`: a fork or a crackle) meets,
    /// its corona's girth and all, a filled cell of a row outside `rows` (a
    /// prompt's powerline bar) or leaves its pane's grid
    /// ([`Self::keep_crackle_off_bars`], [`Self::keep_strike_in_grid`]). A
    /// split's focused pane has its edge inside the window's grid
    /// ([`FrameText::past_pane`]), where the GUI clips the glow to the pane:
    /// read by the window's edge alone, a fork that crossed the divider was
    /// cut there (looks9 round 3's review). The girth is the one each
    /// segment's slabs lay ([`Self::bolt_slab_half`]), across a steep
    /// segment's width and a flat one's height: read as a fifth of a row
    /// across the height alone, a strike's channel (0.55 of a row thick, its
    /// slabs wider on a diagonal) still laid its corona past a split pane's
    /// bottom edge and onto a bar the jump did not cross (looks9's review).
    /// Along its major axis a segment's slabs tile whole pixels, from the
    /// floor of its near end to the ceiling of its far one, so each end is
    /// read a pixel past: read at the ends alone, a steep turn of the
    /// channel in the grid's last pixel row lit it, cut at the card's
    /// bottom edge (1 of 300 seeds of `a_strike_s_corona_keeps_inside_its_grid`).
    #[inline]
    fn bolt_meets_a_bar(
        &self,
        geom: Geom,
        page: u32,
        rows: (u16, u16),
        (pts, branch): (&[(f32, f32)], bool),
    ) -> bool {
        let page = page & 0x00FF_FFFF;
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
        #[cfg(test)]
        let (narrow, ends_unread) = (self.bolt_girth_narrow, self.bolt_ends_unread);
        #[cfg(not(test))]
        let (narrow, ends_unread) = (false, false);
        let corona_th = Self::bolt_corona_th(chf, branch);
        let frame_text = &self.frame_text;
        let (grid_rows, cols) = (geom.rows as i32, geom.cols as i32);
        #[cfg(test)]
        let pane_read = !self.bolt_window_edge;
        #[cfg(not(test))]
        let pane_read = true;
        let (own_lo, own_hi) = (i32::from(rows.0.min(rows.1)), i32::from(rows.0.max(rows.1)));
        let (inv_cwf, inv_chf) = (1.0 / cwf, 1.0 / chf);
        let step_px = (0.5 * cwf).max(1.0);
        let inv_step_px = 1.0 / step_px;
        let on_bar = |x: f32, y: f32| {
            let row = ((y - oyf) * inv_chf).floor() as i32;
            let col = ((x - oxf) * inv_cwf).floor() as i32;
            row < 0
                || row >= grid_rows
                || col < 0
                || col >= cols
                || (pane_read && frame_text.past_pane(row, col))
                || ((row < own_lo || row > own_hi)
                    && matches!(
                        frame_text.pair(row, col),
                        Some((_, ground)) if !super::emit::reads_as_page(ground, page)
                    ))
        };
        let mut wi = 0;
        while wi + 1 < pts.len() {
            let ((xa, ya), (xb, yb)) = (pts[wi], pts[wi + 1]);
            wi += 1;
            let (dx, dy) = (xb - xa, yb - ya);
            // The slabs' half girth and their axis: a steep segment's lie
            // across its width, a flat one's across its height.
            let (half, across_x) = if narrow {
                (chf * 0.15 + 1.0, false)
            } else {
                (
                    Self::bolt_slab_half(corona_th, (dx, dy)) + 1.0,
                    dy.abs() >= dx.abs(),
                )
            };
            // Every cell the girth spans, a half cell apart at most.
            let reads = ((2.0 * half) / (0.5 * cwf.min(chf)).max(1.0))
                .ceil()
                .max(2.0) as usize;
            // The slabs' whole-pixel tiling: a pixel past each end along the
            // major axis, under either girth (the narrow control reverts the
            // girth alone, `bolt_ends_unread` the ends alone).
            let major = dx.abs().max(dy.abs());
            let past = if ends_unread || major < 1e-3 {
                0.0
            } else {
                1.0 / major
            };
            let n = ((major + 2.0 * past * major) * inv_step_px).ceil().max(1.0) as usize;
            let inv_nf = (1.0 + 2.0 * past) / (n as f32);
            let mut k = 0;
            while k <= n {
                let f = k as f32 * inv_nf - past;
                let (x, y) = (xa + dx * f, ya + dy * f);
                let mut j = 0;
                while j <= reads {
                    let off = -half + 2.0 * half * j as f32 / reads as f32;
                    let met = if across_x {
                        on_bar(x + off, y)
                    } else {
                        on_bar(x, y + off)
                    };
                    if met {
                        return true;
                    }
                    j += 1;
                }
                k += 1;
            }
        }
        false
    }

    /// The thickness of a bolt's corona, its widest stroke
    /// ([`Self::emit_bolts`]): a fork's or a crackle's (`branch`), or a
    /// strike's channel.
    #[inline]
    pub(super) const fn bolt_corona_th(ch: f32, branch: bool) -> f32 {
        ch * if branch { 0.30 } else { 0.55 }
    }

    /// The half girth of the slabs a stroke `thickness` px thick lays along
    /// a segment `(dx, dy)` ([`aterm_render::comet_beam`]'s polyline): its
    /// half thickness widened by the segment's slant over its major axis, up
    /// to the square root of two and half a pixel.
    #[inline]
    const fn bolt_slab_half(thickness: f32, (dx, dy): (f32, f32)) -> f32 {
        let half_perp = (thickness * 0.5).max(0.5);
        let seg = crate::color_math::hypot_f32(dx, dy);
        let major = dx.abs().max(dy.abs()).max(1e-3);
        (half_perp * seg / major).min(half_perp * std::f32::consts::SQRT_2 + 0.5)
    }

    /// A bolt's corona's widest half girth ([`Self::bolt_slab_half`] on a
    /// diagonal) and a pixel.
    #[inline]
    const fn bolt_half_girth(ch: f32, branch: bool) -> f32 {
        let half_perp = (Self::bolt_corona_th(ch, branch) * 0.5).max(0.5);
        half_perp * std::f32::consts::SQRT_2 + 0.5 + 1.0
    }

    /// Keep the crackle just spawned (the last bolt) off every filled cell
    /// of another row than the caret's `caret_row` (a prompt's powerline
    /// bar) and inside its pane's grid, its corona's girth and all: laid at
    /// a random angle from a typed cell, it rose onto Solarized Dark's bar
    /// above the typed line as a violet arc apart from the bolt (the
    /// gallery's `context`, looks9 Q8), and one crossing a pane's edge is cut
    /// there to a fragment. Such a crackle is dropped. Turned about its
    /// root's row centre `y0` to aim away (looks9 round 2), the crackle a
    /// pane's bottom edge would have cut rose two rows into the air over the
    /// typed line instead, its root hidden among the letters: a lone violet
    /// zigzag beside the cat's word icon, apart from the caret's bolt (the
    /// gallery's `split` at `b0`, looks9 round 3's Q8).
    #[inline]
    pub(super) fn keep_crackle_off_bars(&mut self, geom: Geom, page: u32, caret_row: u16, y0: f32) {
        let Some(bolt) = self.bolts.last() else {
            return;
        };
        if !bolt.crackle {
            return;
        }
        let chf = geom.ch as f32;
        let half = Self::bolt_half_girth(chf, true);
        let rows = (caret_row, caret_row);
        let laid = bolt.pts;
        if !self.bolt_meets_a_bar(geom, page, rows, (laid.as_slice(), true)) {
            return;
        }
        #[cfg(test)]
        let turned = self.crackle_turned;
        #[cfg(not(test))]
        let turned = false;
        if !turned {
            self.bolts.pop();
            return;
        }
        let mut mirrored = laid;
        let mirrored_slice = mirrored.as_mut_slice();
        let mut mi = 0;
        while mi < mirrored_slice.len() {
            mirrored_slice[mi].1 = 2.0 * y0 - mirrored_slice[mi].1;
            mi += 1;
        }
        let mut flat = laid;
        let laid_slice = laid.as_slice();
        let mut reach = 0.0f32;
        let mut ri = 0;
        while ri < laid_slice.len() {
            reach = reach.max((laid_slice[ri].1 - y0).abs());
            ri += 1;
        }
        let room = (0.5 * chf - half - 0.5).max(0.0);
        let k = if reach > room { room / reach } else { 1.0 };
        let flat_slice = flat.as_mut_slice();
        let mut fi = 0;
        while fi < flat_slice.len() {
            flat_slice[fi].1 = y0 + (flat_slice[fi].1 - y0) * k;
            fi += 1;
        }
        let keep = if !self.bolt_meets_a_bar(geom, page, rows, (mirrored.as_slice(), true)) {
            Some(mirrored)
        } else if !self.bolt_meets_a_bar(geom, page, rows, (flat.as_slice(), true)) {
            Some(flat)
        } else {
            None
        };
        match keep {
            Some(pts) => {
                if let Some(b) = self.bolts.last_mut() {
                    b.pts = pts;
                }
            }
            None => {
                self.bolts.pop();
            }
        }
    }

    /// Keep the strike just forged at `now` (its channel and forks) inside
    /// its pane's grid and off every filled cell of a row the jump did not
    /// cross (`rows`, its start's and its end's), its corona's girth and
    /// all: a fork aimed at a pane's edge was cut there to a lone violet
    /// fragment (the paper gallery's `split`, looks9 round 2's Q8). The
    /// channel's jag is eased toward its chord (between the two cells'
    /// centres) until it meets none, its forks moved
    /// with their roots; a fork that meets one is mirrored about its root's
    /// row to aim away, and dropped if that meets one too. A strike whose
    /// chord's corona leaves the pane is not laid.
    #[inline]
    pub(super) fn keep_strike_in_grid(
        &mut self,
        geom: Geom,
        page: u32,
        rows: (u16, u16),
        now: Instant,
    ) {
        #[cfg(test)]
        if self.strike_forks_cut {
            return;
        }
        let mut first = self.bolts.len();
        while first > 0 {
            let b = &self.bolts[first - 1];
            if b.born == now && !b.crackle {
                first -= 1;
            } else {
                break;
            }
        }
        let mut channel = None;
        let mut ci = self.bolts.len();
        while ci > first {
            ci -= 1;
            if !self.bolts[ci].branch {
                channel = Some(ci);
                break;
            }
        }
        let Some(channel) = channel else {
            return;
        };
        let laid = self.bolts[channel].pts;
        let mut eased = laid;
        let pts = laid.as_slice();
        // The channel is kept by its own corona's girth (0.55 of a row
        // across, [`Self::bolt_corona_th`]). Kept inside its grid by a fork's
        // girth, the channel's corona stood cut at the grid's bottom edge, 3
        // px of lavender along the settings card's last pixel row under the
        // Return's strike (looks10 round 3, after main's card edge law); its
        // negative control (`strike_channel_fork_girth`) reads it as a fork.
        #[cfg(test)]
        let channel_as_fork = self.strike_channel_fork_girth;
        #[cfg(not(test))]
        let channel_as_fork = false;
        if let (Some(&a), Some(&b)) = (pts.first(), pts.last()) {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let inv = 1.0 / (dx * dx + dy * dy).max(1e-3);
            let mut k = 1.0f32;
            while k > 0.0
                && self.bolt_meets_a_bar(geom, page, rows, (eased.as_slice(), channel_as_fork))
            {
                k = (k - 0.25).max(0.0);
                let eased_slice = eased.as_mut_slice();
                let mut pi = 0;
                while pi < eased_slice.len() {
                    let p = pts[pi];
                    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) * inv).clamp(0.0, 1.0);
                    let chord = (a.0 + dx * t, a.1 + dy * t);
                    eased_slice[pi] =
                        (chord.0 + (p.0 - chord.0) * k, chord.1 + (p.1 - chord.1) * k);
                    pi += 1;
                }
            }
            if self.bolt_meets_a_bar(geom, page, rows, (eased.as_slice(), channel_as_fork)) {
                // Even the chord's corona leaves the pane (a jump down the
                // pane's first or last column, the channel's slabs wider
                // than half a cell): no strike.
                self.bolts.truncate(first);
                return;
            }
            self.bolts[channel].pts = eased;
        }
        let mut i = self.bolts.len();
        while i > first {
            i -= 1;
            if i == channel {
                continue;
            }
            let mut fork = self.bolts[i].pts;
            let root = fork.as_slice().first().copied();
            if let Some(r) = root {
                let mut at = None;
                let mut pi = 0;
                while pi < pts.len() {
                    if pts[pi] == r {
                        at = Some(pi);
                        break;
                    }
                    pi += 1;
                }
                if let Some(at) = at {
                    let (to, from) = (eased.as_slice()[at], pts[at]);
                    let (tx, ty) = (to.0 - from.0, to.1 - from.1);
                    let fork_slice = fork.as_mut_slice();
                    let mut fi = 0;
                    while fi < fork_slice.len() {
                        let p = fork_slice[fi];
                        fork_slice[fi] = (p.0 + tx, p.1 + ty);
                        fi += 1;
                    }
                }
            }
            if self.bolt_meets_a_bar(geom, page, rows, (fork.as_slice(), true)) {
                let y0 = fork.as_slice().first().map_or(0.0, |p| p.1);
                let fork_slice = fork.as_mut_slice();
                let mut fi = 0;
                while fi < fork_slice.len() {
                    fork_slice[fi].1 = 2.0 * y0 - fork_slice[fi].1;
                    fi += 1;
                }
                if self.bolt_meets_a_bar(geom, page, rows, (fork.as_slice(), true)) {
                    self.bolts.remove(i);
                    continue;
                }
            }
            self.bolts[i].pts = fork;
        }
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
    #[inline]
    pub(super) fn meteor_strike_fountain(&mut self, m: &Meteor, now: Instant, geom: Geom) {
        let cell = geom.ch as f32;
        let (dx, dy) = (m.x1 - m.x0, m.y1 - m.y0);
        let len = crate::color_math::hypot_f32(dx, dy).max(1.0);
        let inv_len = 1.0 / len;
        let dist_cells = len / geom.cw.max(1) as f32;
        let (ux, uy) = (dx * inv_len, dy * inv_len);
        let burst = ((6.0 + dist_cells) as usize).min(24);
        let mut bi = 0;
        while bi < burst {
            bi += 1;
            if self.particles.len() >= Self::MAX_PARTICLES {
                break;
            }
            let r0 = self.frand();
            let r1 = self.frand();
            let r2 = self.frand();
            let r3 = self.frand();
            let r4 = self.frand();
            let t = 1.0 - Self::STRIKE_DEBRIS_REACH * (1.0 - crate::color_math::sqrt_f32(r0));
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
                latch: ParticleLatch::FRESH,
                edge_cut: false,
            });
        }
    }

    #[inline]
    pub(super) fn poof_scan(&mut self, now: Instant, cfg: &GlowConfig, geom: Geom) {
        if self.kill_hint.is_none() && self.bs_poof_hint.is_none() {
            self.rotate_row_probe();
            return;
        }
        // Expire a stale hint: it keeps the animation timer armed
        // (`is_active`), so an unconsumed one must self-disarm.
        if let Some(t) = self.kill_hint
            && now.saturating_duration_since(t) > KILL_HINT_FRESH_DUR
        {
            self.kill_hint = None;
        }
        if let Some(t) = self.bs_poof_hint
            && now.saturating_duration_since(t) > KILL_HINT_FRESH_DUR
        {
            self.bs_poof_hint = None;
            self.bs_baseline = None;
        }
        if self.kill_hint.is_none() && self.bs_poof_hint.is_none() {
            self.rotate_row_probe();
            return;
        }
        let fresh_kill = true;
        let gap_ok = match self.last_poof {
            Some(t) => now.saturating_duration_since(t) >= POOF_MIN_GAP_DUR,
            None => true,
        };
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
            && now.saturating_duration_since(prev.at) <= POOF_PROBE_STALE_DUR
        {
            let cur_fill = cur.fill as usize;
            let prev_fill = prev.fill as usize;
            let prev_slice = &self.row_prev[..prev_fill.min(self.row_prev.len())];
            let cur_slice = &self.row_cur[..cur_fill.min(self.row_cur.len())];
            let p_cap = cur_fill.min(prev_slice.len()).min(cur_slice.len());
            let mut p = 0;
            while p < p_cap && prev_slice[p] == cur_slice[p] {
                p += 1;
            }
            let mut s = 0;
            while s < p_cap
                && prev_slice[prev_slice.len() - 1 - s] == cur_slice[cur_slice.len() - 1 - s]
            {
                s += 1;
            }
            if cur_fill > 0 && p + s >= cur_fill {
                let s_span = s.min(cur_fill - p.min(cur_fill));
                let c0 = p.min(prev.caret as usize).min(cur_fill) as u16;
                let c1 = (prev_fill - s_span).max(c0 as usize + 1) as u16;
                let mut survivor_holds_ink = s_span > 0;
                if !survivor_holds_ink {
                    let prefix_end = (c0 as usize).min(cur_slice.len());
                    let mut ci = 0;
                    while ci < prefix_end {
                        if cur_slice[ci] != ' ' {
                            survivor_holds_ink = true;
                            break;
                        }
                        ci += 1;
                    }
                }
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
            (Some(a), None) => Some(a),
            (None, b) => b,
        };
        let erasure_proven = matches!(
            (self.row_prev_meta, self.row_cur_meta),
            (Some(prev), Some(cur)) if prev.row == cur.row && cur.fill < prev.fill
        );
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
                || match poof_hint_at {
                    Some(t) => now.saturating_duration_since(t) >= POOF_FALLBACK_GRACE_DUR,
                    None => false,
                })
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
    pub(super) const fn rotate_row_probe(&mut self) {
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
    #[inline]
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
                let inv_drops = 1.0 / (drops as f32);
                let gy = if comet { 0.25 * cell } else { 3.0 * cell };
                let mut k = 0;
                while k < drops {
                    if self.particles.len() >= Self::MAX_PARTICLES {
                        break;
                    }
                    let r0 = self.frand();
                    let r1 = self.frand();
                    let r2 = self.frand();
                    let r3 = self.frand();
                    let r4 = self.frand();
                    self.particles.push(Particle {
                        x0: span_x0 + ((k as f32 + 0.5) * inv_drops) * span_w + (r0 - 0.5) * cwf,
                        y0: oy,
                        vx: (r1 - 0.5) * 1.4 * cell,
                        vy: -(0.5 + r2 * 0.9) * cell,
                        gy,
                        life: if comet {
                            0.45 + r3 * 0.6
                        } else {
                            0.3 + r3 * 0.35
                        },
                        hue: r4,
                        cov_scale: 1.0,
                        born: now,
                        latch: ParticleLatch::FRESH,
                        edge_cut: false,
                    });
                    k += 1;
                }
            }
            GlowStyle::Fire => {
                let puffs = ((1.0 + crate::color_math::sqrt_f32(nf)) as usize).clamp(1, 5);
                let inv_puffs = 1.0 / (puffs as f32);
                let x_at = |k: usize| span_x0 + ((k as f32 + 0.5) * inv_puffs) * span_w;
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
    #[inline]
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
        let inv_stars = 1.0 / (stars as f32);
        let speed = 0.55 + 0.85 * mom.clamp(0.0, 1.0);
        let heft = erase_weight(n) * (1.0 / ERASE_WEIGHT_CAP);
        let spread_w = span_w.min(cwf * ERASE_POOF_SPREAD_CELLS);
        let heft_y = 0.6 + 0.9 * heft;
        let gy = 0.9 * cell;
        let life_scale = 0.85 + 0.45 * heft;
        let mut k = 0;
        while k < stars {
            if self.particles.len() >= Self::MAX_PARTICLES {
                break;
            }
            let r0 = self.frand();
            let r1 = self.frand();
            let r2 = self.frand();
            let r3 = self.frand();
            let r4 = self.frand();
            let f = (k as f32 + 0.5) * inv_stars;
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
                latch: ParticleLatch::FRESH,
                edge_cut: false,
            });
            k += 1;
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
                latch: ParticleLatch::FRESH,
                edge_cut: false,
            });
        }
    }

    /// The neutral-grey smoke poof puffs.
    #[allow(
        clippy::too_many_arguments,
        reason = "span + geometry scalars; two internal call sites (spawn_poof)"
    )]
    #[inline]
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
        let puffs = ((1.0 + crate::color_math::sqrt_f32(f32::from(n)) * 1.4) as usize).clamp(1, 8);
        let inv_puffs = 1.0 / (puffs as f32);
        let spread_w = span_w.min(cwf * ERASE_POOF_SPREAD_CELLS);
        let mut k = 0;
        while k < puffs {
            if self.vapor.len() >= Self::MAX_VAPOR {
                break;
            }
            let (r0, r1, r2) = (self.frand(), self.frand(), self.frand());
            let f = (k as f32 + 0.5) * inv_puffs;
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
            k += 1;
        }
    }

    /// Quench steam ([`Self::update_typing_thermals`], [`Self::spawn_poof`]).
    #[inline]
    pub(super) fn spawn_quench_steam(
        &mut self,
        puffs: usize,
        x_at: impl Fn(usize) -> f32,
        oy: f32,
        cell: f32,
        cw: f32,
        now: Instant,
    ) {
        let mut k = 0;
        while k < puffs {
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
            k += 1;
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
