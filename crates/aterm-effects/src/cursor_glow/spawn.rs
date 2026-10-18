// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SPAWNING — the light, particles, rings, bolts and poofs a licensed move
//! or an erase mints into the engine's state.

use super::*;

// ---- the erase poof's energy -------------------------------------------------
//
// The delete poof scales with two independent terms, multiplied
// (owner, 2026-08-06):
//
//   * momentum: how hard the delete run is going, from
//     [`CursorGlow::erase_mom`], the mirror of the canonical typing metric;
//   * weight: how much vanished, in cells.
//
// So a lone Backspace, a held delete run and a killed line poof differently.
/// Floor of the momentum term, deliberately high: a single cold delete is
/// still clearly acknowledged, and momentum carries it from there.
pub(super) const ERASE_MOM_FLOOR: f32 = 0.55;
/// Ceiling on the weight term. `sqrt(cells)` grows the burst with what
/// vanished (1 cell → 1.0, a six-letter word → 2.4, a killed line → this cap)
/// without a full-line kill scaling into a screen-filling cloud.
pub(super) const ERASE_WEIGHT_CAP: f32 = 3.4;
/// Sparkles at drive 1.0. A cold single-character Backspace (drive =
/// [`ERASE_MOM_FLOOR`]) throws a handful of grains, a hot word kill lands
/// mid-band, and a hot line kill hits [`ERASE_POOF_CAP`].
pub(super) const ERASE_POOF_STARS: f32 = 8.0;
/// Hard ceiling on one poof's sparkle count, well under `MAX_PARTICLES`, so a
/// held Ctrl-U mash cannot crowd out the rest of the family, with headroom for
/// a line kill over a word kill.
pub(super) const ERASE_POOF_CAP: usize = 18;
/// How wide, in cells, one poof's debris may spread regardless of the erased
/// span. A long span spread evenly would read as a dotted rule; the span sets
/// the poof's energy (via [`erase_weight`]) but not its geometry. Debris
/// clusters at the collapse point (the span's start, where the caret ends up)
/// and thins outward.
pub(super) const ERASE_POOF_SPREAD_CELLS: f32 = 9.0;
/// How much headroom above the erased row the debris needs before it is allowed
/// to rise. Inside this, the poof falls instead — see `spawn_erase_sparkles`.
pub(super) const ERASE_POOF_LIFT_CELLS: f32 = 1.2;
/// The hero grain's `cov_scale` band, at the top of the glitter sentinel range
/// (`< 0.9`): `MIN` at a single character, `MIN + SPAN` at a killed line.
/// `emit_particles` decodes it into a size multiplier, so the poof's central
/// pop grows with the weight of what vanished.
pub(super) const ERASE_HERO_SCALE_MIN: f32 = 0.80;
pub(super) const ERASE_HERO_SCALE_SPAN: f32 = 0.09;
/// The round-spark band, at the bottom of the glitter sentinel range: the
/// poof's body grains spawn at [`ERASE_ROUND_SCALE`], and `emit_particles`
/// draws anything under [`ERASE_ROUND_SCALE_MAX`] as a round point of cool
/// light, not a 4-point plus. A cloud of small crosses at glyph size would read
/// as literal '+' characters in the buffer, so the poof keeps exactly one plus
/// (the hero band above). Other glitter-band grains fall through to the
/// population deal in `emit_particles` (round [`dust_r`] stardust unless dealt
/// the 1-in-[`crate::effect_util::STAR_ACCENT_DEN`] accent, see
/// [`star_accent`]).
pub(super) const ERASE_ROUND_SCALE: f32 = 0.5;

/// The WEIGHT term: how big the vanished span reads, in
/// `1.0..=`[`ERASE_WEIGHT_CAP`]. Monotone non-decreasing in `cells` (pinned by
/// `erase_poof_drive_grows_with_momentum_and_weight`).
#[inline]
pub(super) fn erase_weight(cells: u16) -> f32 {
    f32::from(cells.max(1)).sqrt().min(ERASE_WEIGHT_CAP)
}

/// The poof's energy: the momentum term (floored at [`ERASE_MOM_FLOOR`]) times
/// the [`erase_weight`] of what vanished. Monotone non-decreasing in both
/// arguments.
#[inline]
pub(super) fn erase_poof_drive(mom: f32, cells: u16) -> f32 {
    (ERASE_MOM_FLOOR + (1.0 - ERASE_MOM_FLOOR) * mom.clamp(0.0, 1.0)) * erase_weight(cells)
}

impl CursorGlow {
    /// A real jump slams the flare and forgets the rainbow kitty tail memory; the
    /// abandoned ribbon, a backward TUI re-anchor's stranded ribbon, and a TUI
    /// repaint's OFF-ROW stranded ribbon are retired so stale light never parks
    /// over cells the typing left.
    pub(super) fn retire_abandoned_light(&mut self, mv: &MoveCtx) {
        let &MoveCtx {
            typing,
            fire,
            navigation,
            ..
        } = mv;
        // A NON-TYPING move flares the head — except a fire jump (its meteor
        // carries the drama) and a pure navigation move (T1: no light).
        if !typing && !fire && !navigation {
            self.flare = 1.0;
        }
    }

    /// The aural twin of this spawn: cue the sound the classification earned,
    /// debiting the key-time click ledger only on the arms a printable key's
    /// echo can land on.
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
        // Sound: the aural twin of this spawn, with the classification the light
        // computed (after the heat/flare update). Kill cues fire at the poof site
        // instead, since a kill's echo may not move the caret.
        //
        // An admitted navigation move sings in the melody: a short scrub glides
        // one in-key step, a fast run sweeps a short scale, both carrying
        // direction.
        //
        // One keypress, one click, debited only by the arms a printable key's
        // echo can land on (`typing` and the `else` Jump), never by `deletion`
        // or `navigation`. Those two are hint-only, so a printable echo overtaken
        // by the next key's hint can misclassify into them; if the credit were
        // retired there, the echo would later fall through to `typing` with an
        // empty ledger and click spuriously (`x` then Backspace would make three
        // sounds). The trade-off, a swallowed echo leaving a credit that mutes
        // one later click until `KEYED_CLICK_FRESH`, is milder.
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
            // One character, one click: if the host already clicked this glyph at
            // the physical keypress ([`Self::cue_keystroke`]), spend that credit
            // and stay silent. An advance with no credit (pure PTY output, a host
            // without the key seam, an echo past the freshness window) clicks
            // here.
            if !self.take_keyed_click(now) {
                self.cue_sound(crate::trail_sound::SoundKind::Typed, cc);
            }
        } else {
            // A printable keystroke whose echo moves the cursor more than one
            // cell (vim motions, keys in less/htop/fzf, a multi-cell IME commit)
            // classifies as a Jump, so the key credit is spent here too.
            // Otherwise one keypress speaks twice (`Jump` bypasses MIN_GAP), and
            // the unspent credit would later mute a real click. A Jump with no
            // credit (pure PTY output, a program moving the cursor) still cues.
            if !self.take_keyed_click(now) {
                self.cue_sound(crate::trail_sound::SoundKind::Jump, cc);
            }
        }
    }

    /// Per-style birth brightness for a typing spark (1.0 on the jump path).
    pub(super) fn birth_boost(&self, mv: &MoveCtx) -> f32 {
        let &MoveCtx { cfg, typing, .. } = mv;
        if typing {
            match cfg.style {
                // Phaser: constant brightness at any cadence (its
                // constant-distance promise), so no heat ramp.
                GlowStyle::Phaser => 1.0,
                // Fire brightens on the eased display temperature: the coal floor
                // keeps human-cadence typing alight and the ramp swells.
                GlowStyle::Fire => 0.28 + 0.92 * self.fire_t(),
                // Beam: steady light has one brightness, so the first keys of a
                // run do not read as a dim smear.
                GlowStyle::Beam => 1.0,
                _ => self.typing_boost(),
            }
        } else {
            1.0
        }
    }

    /// Per-style spark lifetime for this move: typing wakes chain to the
    /// observed cadence, jumps take the configured full life (the phaser jump
    /// a brisk clamp).
    pub(super) fn move_spark_life(&self, mv: &MoveCtx, gap: f32) -> f32 {
        let &MoveCtx { cfg, typing, .. } = mv;
        // Typing gets a short tight wake (~100 ms) so the cursor reads as
        // leading; a real jump gets the full comet duration, so a long jump trail
        // does not smear typing. Heat stretches the typing wake (up to ~2.3×).
        // During a rhythm (inter-key gap ≤ CHAIN_GAP_MAX) a spark's life also
        // chains to the observed cadence (gap + margin, capped), so the wake is
        // continuous; the first key of a burst (gap = ∞) takes no floor.
        let full_life = cfg.duration.as_secs_f32().max(0.001);
        if typing {
            match cfg.style {
                // Phaser: a constant-distance streak. The band always spans the
                // last two typed cells fully lit: life is
                // [`Self::PHASER_CHAIN_KEYS`]× the inter-key gap (+ fade margin),
                // not heat-stretched. Only a burst's first key takes the base
                // time; the window and cap are generous so slow rhythms chain.
                GlowStyle::Phaser => {
                    if gap <= Self::PHASER_CHAIN_GAP_MAX {
                        (Self::PHASER_CHAIN_KEYS * gap + Self::CHAIN_MARGIN)
                            .min(Self::PHASER_CHAIN_LIFE_MAX)
                    } else {
                        (full_life * 2.0).clamp(0.40, 0.75)
                    }
                }
                // Water at speed stretches its wake up to ~4× so the deep palette
                // reads as a trail of water, not a stub.
                GlowStyle::Water => (full_life * 0.42).clamp(0.06, 0.14) * (1.0 + 3.0 * self.heat),
                // Fire burns on: embers die down, leaving a trail of fire. The
                // ceiling honours a long `cursor_trail_ms` (up to ~0.45 s base,
                // ~1.4 s at full heat) while the early-fade envelope keeps the
                // old tail ember-dim. Chained to cadence like the default arm.
                GlowStyle::Fire => {
                    // Life stretches on the eased temperature (coal-floored),
                    // keeping embers alive past one inter-key gap.
                    let heat_life =
                        (full_life * 0.38).clamp(0.10, 0.60) * (1.0 + 2.5 * self.fire_t());
                    if gap <= Self::CHAIN_GAP_MAX {
                        heat_life.max((gap + Self::CHAIN_MARGIN).min(Self::CHAIN_LIFE_MAX))
                    } else {
                        heat_life
                    }
                }
                // Beam: a steady tube whose typed span is cadence-scaled
                // ([`Self::BEAM_CHAIN_KEYS`] inter-key gaps, about 3.5 letters),
                // with `beam_power`'s full-power hold keeping the newest letters
                // lit. A burst's first key takes the base time.
                GlowStyle::Beam => {
                    if gap <= Self::BEAM_CHAIN_GAP_MAX {
                        (Self::BEAM_CHAIN_KEYS * gap + Self::CHAIN_MARGIN)
                            .min(Self::BEAM_CHAIN_LIFE_MAX)
                    } else {
                        (full_life * 0.5).clamp(0.13, 0.40)
                    }
                }
                // Lightning stays charged: the typed path honours a long
                // `cursor_trail_ms` (ceiling 0.40 s base, ~1.1 s at full heat)
                // while `emit_comet`'s discharge envelope bleeds aged cells to a
                // dim flickering residual. Chained to cadence like the default.
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
            // The phaser jump streak is a brisk swoosh; the clamp keeps a long
            // `cursor_trail_ms` from parking the fat bar over the swept line.
            (full_life * 0.5).clamp(0.30, 0.90)
        } else {
            full_life
        }
    }

    /// Whether this spark is a resident, i.e. survives this frame's decay pass.
    /// `tick` spawns before it reaps, so a producer asking "is this cell already
    /// lit?" must ask exactly the reaper's question: looser leaves a hole in the
    /// mark, tighter lays a duplicate spark on an occupied cell.
    #[inline]
    pub(super) fn spark_resident_at(now: Instant, s: &Spark) -> bool {
        now.saturating_duration_since(s.born).as_secs_f32() < s.life
    }

    /// The per-move light: exactly one of the style arms (fire meteor, rainbow
    /// kitty jump, the navigation no-wake arm, the rainbow kitty backspace
    /// poof, or the swept comet/ribbon path). Returns how many hue steps this
    /// move advances (a coalesced typing run counts its laid cells; other moves
    /// one).
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
            let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
            let (x1, y1) = geom.cell_center(cr, cc);
            // Multi-hop coalescing: shell repaints hop the cursor through parked
            // intermediate positions within a frame or two. A fresh meteor whose
            // origin is a young live meteor's landing retargets that meteor in
            // place (one choreography, one streak). Every live meteor is searched,
            // newest first, since up to METEOR_CAP can overlap. The anchor must
            // convert to window space exactly as the landing does.
            let (o_x, o_y) = geom.cell_center(pr, pc);
            let retarget = self.fire_meteors.iter_mut().rev().find(|m| {
                now.saturating_duration_since(m.born).as_secs_f32() <= Self::METEOR_RETARGET_S
                    && (m.x1 - o_x).abs() < cwf * 0.6
                    && (m.y1 - o_y).abs() < chf * 0.6
            });
            // Long enough that the flight spans more than one or two frames,
            // under half a second overall.
            let life = (0.16 + 0.005 * dist).clamp(0.20, 0.45);
            if let Some(m) = retarget {
                m.x1 = x1;
                m.y1 = y1;
                m.life = m.life.max(life);
                // Restart the flight and the retarget window from this hop, so a
                // lagging choreography (a prompt reprint can lag ~100 ms) keeps
                // chaining into one streak.
                m.born = now;
                m.arrived = false; // the strike now happens at the NEW landing
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
                    // Momentum floor: a cold Ctrl-A/E still throws a bright
                    // streak; hot runs sit above it.
                    mom: self.fire_t().max(0.55),
                    arrived: false,
                });
            }
        } else if navigation && (!water || dist < 2.0) {
        } else {
            let cap = Self::MAX_SPARKS;
            // Laser, beam and water jumps ignore the configured typing-tail
            // length: their streak spans the whole leap. The resident cap still
            // bounds the destination-nearest suffix of an outlier vector.
            let max_len = if matches!(
                cfg.style,
                GlowStyle::Laser | GlowStyle::Beam | GlowStyle::Water
            ) && !typing
            {
                cap
            } else {
                cfg.length.clamp(1, cap)
            };
            self.path_scratch.clear();
            if shape_wrap {
                // Non-rainbow-kitty wrap run: the trail lives behind the cursor,
                // so lay the typed cells the echo swept through the fold (the
                // old row's last cells and glyphs already on the new row,
                // excluding the live caret cell), not a diagonal across skimmed
                // cells nor just the empty cursor cell. Bounded by the wrap shape
                // guards: ≤4 tail + ≤3 head cells.
                let pane_col1 = pane_col0.saturating_add(pane_cols);
                for c in (pc as i32)..(pane_col1 as i32) {
                    self.path_scratch.push((pr as i32, c));
                }
                for c in (pane_col0 as i32)..(cc as i32) {
                    self.path_scratch.push((cr as i32, c));
                }
            } else if re_anchor {
                // TUI re-anchor (non-rainbow-kitty): the repaint relocated the
                // caret, so only the landing gets a wake. (A fold carrying
                // admitted typed intent takes the wrap-run arm above.)
                self.path_scratch.push((cr as i32, cc as i32));
            } else {
                // A real jump, or a coalesced echo (2-3 glyphs in one observed
                // move, collapsed to typing by `echo_run`), lays the trail behind
                // the cursor. For a short echo `line_cells_tail` lays every swept
                // cell gap-free, excluding the caret cell, each a typed head in
                // successive hue via `typing_run`. The echo must classify as
                // typing: jump sparks would interleave and gap the trail.
                line_cells_tail(
                    &mut self.path_scratch,
                    (pr as i32, pc as i32),
                    (cr as i32, cc as i32),
                    max_len,
                    false,
                );
            }
            // Fire stays on one line: a row change (Enter, wrap, arrow down, a
            // click) leaves no fire on the line you left. The swept path keeps
            // only its landing-row segment, and every spark and ember on another
            // row is snuffed (life clamped to a fast die-down, so the old line
            // gutters out). The jump flare, landing ring and ember fountain still
            // announce the leap.
            //
            // A backward same-row leap is treated the same way: at the bottom
            // row an Enter scrolls instead of moving down and arrives as a jump
            // back to column 0. Forward same-row leaps still sweep.
            //
            // The phaser shares this discipline: its trail is the last three
            // letters, so navigation never drags or strands the bar.
            let fire_back_leap = !typing && cc < pc && pr == cr;
            // Exemptions for the phaser: a typing wrap (the band follows the
            // typing through the fold; the pre-wrap tail fades on its own life)
            // and a settled resize (the band shows where the cursor's home
            // moved; it rides the same one-shot license as every style). Fire
            // keeps its one-line rule even on a wrap.
            let phaser_wrap_follow = matches!(cfg.style, GlowStyle::Phaser) && typing;
            if matches!(cfg.style, GlowStyle::Fire | GlowStyle::Phaser)
                && (dr_abs >= 1 || fire_back_leap)
                && !phaser_wrap_follow
                && !reflow_licensed
            {
                /// Seconds an off-line flame gets to gutter out after the
                /// cursor leaves it behind.
                const SNUFF: f32 = 0.18;
                if fire_back_leap || matches!(cfg.style, GlowStyle::Phaser) {
                    // The phaser keeps no landing segment on a row change: the
                    // band belongs to typed letters only.
                    self.path_scratch.clear();
                } else {
                    self.path_scratch.retain(|&(r, _)| r == cr as i32);
                }
                let chf = geom.ch as f32;
                let snuffed = |row: u16, col: u16| {
                    row != cr || (fire_back_leap && col > cc.saturating_add(1))
                };
                for s in &mut self.sparks {
                    if snuffed(s.row, s.col) {
                        let age = now.saturating_duration_since(s.born).as_secs_f32();
                        s.life = s.life.min(age + SNUFF);
                    }
                }
                for p in &mut self.particles {
                    let prow = (p.y0 / chf) as i32;
                    if prow != cr as i32 || fire_back_leap {
                        let age = now.saturating_duration_since(p.born).as_secs_f32();
                        p.life = p.life.min(age + SNUFF);
                    }
                }
            }
            if !self.path_scratch.is_empty() {
                // A coalesced typing run treats every swept cell as a typed head.
                // Ancillary hue advances one step per cell; classic positions are
                // assigned together by the run reflow after spawning.
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
                    // Tail→head grade 0..1, except for typing runs, whose cells a
                    // per-key observer would each have laid at pos 1.0; grading
                    // them would burn a dimmer notch into the band.
                    let pos = if typing_run {
                        1.0
                    } else {
                        (i as f32 + 1.0) / n // tail→head 0..1
                    };
                    // Faint tail → bright head, scaled by typing heat (jump = 1×),
                    // clamped into u8. Laser carries near-constant power (tail
                    // ~80%) and fires the head at full 255.
                    let born_cov = match cfg.style {
                        GlowStyle::Laser => ((205.0 + 50.0 * pos) * boost).min(255.0) as u8,
                        // Beam: near-constant power (tail ~70%), topping out
                        // under the laser's maximum.
                        GlowStyle::Beam => ((165.0 + 70.0 * pos) * boost).min(240.0) as u8,
                        // Phaser: near-constant power (tail ~80%), so the band
                        // reads as one bar of spectrum; a vivid tint the glyphs
                        // render through.
                        GlowStyle::Phaser => ((92.0 + 34.0 * pos) * boost).min(150.0) as u8,
                        // Fire: the baseline vein stays a warm tint the words
                        // render over; the drama is the curtain above the line
                        // (`emit_flames`) and the ember populations.
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
                        // Ancillary birth hue. Phaser renders this field;
                        // RainbowKitty retains it for compatibility clocks and
                        // seeds but renders `classic_t` instead.
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

    /// Laser: a jump forges a strike along the jump vector; sustained hot
    /// typing crackles short stray arcs off the charged trail.
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
        // Lightning (laser): a real jump is a strike (a jagged main channel with
        // branch forks), and sustained hot typing crackles short arcs off the
        // head. Geometry is forged once here (deterministic rng); only
        // brightness animates. Navigation-blind like the ring/particle gates:
        // Ctrl-A/E and Home/End earn no strike or crackle.
        if matches!(cfg.style, GlowStyle::Laser) && !navigation {
            let chf = geom.ch as f32;
            if dist >= 2.0 {
                self.spawn_bolt(
                    geom.cell_center(pr, pc),
                    geom.cell_center(cr, cc),
                    chf,
                    true,
                    now,
                );
            } else if self.heat > 0.30 && self.frand() < 0.18 + 0.55 * self.heat {
                // Crackle: a short stray arc in a random direction, only while
                // hot, rooted at a random live typing spark among the last
                // [`Self::CRACKLE_TRAIL_CELLS`], so the residual charge sparks
                // behind the cursor.
                let pick = self.frand();
                let a = self.frand() * std::f32::consts::TAU;
                let len = (1.6 + self.frand() * 2.8) * chf;
                let (rr, rc) = {
                    let live = |s: &&Spark| {
                        s.typing && now.saturating_duration_since(s.born).as_secs_f32() < s.life
                    };
                    let trail = self
                        .sparks
                        .iter()
                        .rev()
                        .take(Self::CRACKLE_TRAIL_CELLS)
                        .filter(live);
                    let n = trail.clone().count();
                    if n == 0 {
                        (cr, cc)
                    } else {
                        let i = ((pick * n as f32) as usize).min(n - 1);
                        let s = trail
                            .clone()
                            .nth(i)
                            .expect("index bounded by the trail count");
                        (s.row, s.col)
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

    /// Landing ring on a real jump (fire meteors ping at ARRIVAL instead).
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
        // Landing ring on a real jump (>1 cell), if enabled; fire meteors ring
        // at arrival instead (see `tick`).
        if cfg.ring && dist >= 2.0 && !fire_meteor && !navigation {
            let (cx, cy) = geom.cell_center(cr, cc);
            let scale = crate::rainbow_kitty::timing::impact(dist);
            self.ring = Some(Ring {
                cx,
                cy,
                born: now,
                life: classic_ring_life(0.18, scale),
                scale,
            });
        }
    }

    /// The per-style particle burst for this move: jump-scaled showers,
    /// heat-scaled typing sheds; fire meteor moves defer to the strike.
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
            ..
        } = mv;
        // Particles (sparkle / fire / water / laser): a burst scaled by jump
        // distance, or by heat when typing (a lone key sheds one ember or
        // droplet, a fast run a shower). Water has the widest range. Fire
        // meteor moves defer their debris to the strike
        // (`meteor_strike_fountain`), along the final coalesced path.
        if cfg.style.has_particles() && !fire_meteor && !navigation {
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
                    // A jump erupts a fountain scaled by the leap, born along the
                    // swept path so the whole line catches.
                    ((12.0 + dist * 1.8) as usize).min(44)
                } else if laser {
                    // The beam LANDS like a cutting torch: a hard ablation shower.
                    ((8.0 + dist * 1.3) as usize).min(30)
                } else if beam {
                    // A warp leap stirs a modest wake of STARDUST along the rod —
                    // stars, not exhaust; the rod itself is the show.
                    ((4.0 + dist * 0.5) as usize).min(14)
                } else if comet {
                    // A leap sheds a meteor train along the swept vector,
                    // restrained so the beam stays the focus.
                    ((5.0 + dist * 0.9) as usize).min(18)
                } else if matches!(cfg.style, GlowStyle::Sparkle) {
                    // A leap spills a generous train of stars, moons and
                    // mini-comets along the vector.
                    ((9.0 + dist * 1.3) as usize).min(32)
                } else {
                    (6.0 + dist).min(20.0) as usize
                }
            } else if water {
                1 + (self.heat * 6.0) as usize
            } else if fire {
                // ONE smoldering mote at a slow peck; a roaring ember COLUMN at
                // full key-repeat blaze — the fire visibly feeds on typing speed.
                1 + (blaze * 13.0) as usize
            } else if laser {
                // A cold beam cuts CLEAN (no sparks for a lone keystroke); only a
                // sustained fast run grinds a spark shower off the page.
                (self.heat * 5.0) as usize
            } else if beam {
                // Space is never empty: even a stroll leaves the odd star in the
                // wake; a warp-speed run streams a field of them.
                1 + (self.heat * 4.0) as usize
            } else if comet {
                // A quiet nucleus sheds one grain; a fast run outgasses glitter,
                // count-capped under fire's ember column.
                1 + (self.heat * 4.0) as usize
            } else if matches!(cfg.style, GlowStyle::Sparkle) {
                // Even a slow peck drops a couple of celestial grains; a hot
                // run pours the constellation out of the ribbon.
                2 + (self.heat * 6.0) as usize
            } else {
                1 + (self.heat * 3.0) as usize
            };
            let (ox, oy) = geom.cell_center(cr, cc);
            let cell = geom.ch as f32;
            let hue0 = self.hue;
            for _ in 0..burst {
                // Draw all randomness into locals FIRST (each `frand` borrows self),
                // then build the particle — no borrow overlap with `self.particles`.
                let r0 = self.frand();
                let r1 = self.frand();
                let r2 = self.frand();
                let r3 = self.frand();
                let r4 = self.frand();
                let r5 = self.frand();
                let p = match cfg.style {
                    GlowStyle::Fire => {
                        // Three ember populations (the `hue` seed doubles as
                        // temperature and size, see `emit_particles`):
                        //   pop     - a white-hot spark shot fast and high, dying
                        //             young; only when truly hot;
                        //   smolder - a slow dim mote that drifts and lingers;
                        //   ember   - the buoyant riser, kicked harder with heat.
                        // On a jump each ember is born along the leap (`r0` walks
                        // the vector), so the whole path catches fire.
                        let (bx, by) = if dist >= 2.0 {
                            let (fx, fy) = geom.cell_center(pr, pc);
                            (fx + (ox - fx) * r0, fy + (oy - fy) * r0)
                        } else {
                            (ox + (r0 - 0.5) * geom.cw as f32, oy)
                        };
                        // Jump embers die in half the time, so the landing row is
                        // quiet again well under a second.
                        let jl = if dist >= 2.0 { 0.5 } else { 1.0 };
                        let pop_p = 0.25 * blaze;
                        let smolder_p = 0.55 - 0.35 * blaze;
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
                        // A jump is a splash instead: crown droplets arcing over
                        // plus a fast low skim across the surface.
                        let heat = self.heat;
                        if dist >= 2.0 {
                            if r5 < 0.4 {
                                // Low skim: fast, flat, short-lived.
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
                                // Crown splash: up and out, arcing over.
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
                            let bubble_p = if heat > 0.45 { 0.30 } else { 0.0 };
                            let drip_p = 0.75 - 0.55 * heat;
                            if r5 < bubble_p {
                                // Bubble: born in the wake behind the head,
                                // drifting up under buoyancy.
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
                                // Drip: clings under the baseline, then falls —
                                // deep enough (~a row) to read before it dies.
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
                                // Spray: heat-scaled energy.
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
                        // Ablation sparks: hard, fast, short-lived, ejected with a
                        // sideways bias from the impact and pulled down by
                        // gravity; drawn as velocity-aligned streaks.
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
                            // Exact-point births (the cutting-torch read), so the
                            // shower divides its punch: 1/√n keeps a lone spark at
                            // full brightness while a 30-spark landing stacks to
                            // the beam hue rather than white.
                            cov_scale: (burst as f32).sqrt().recip(),
                            born: now,
                        }
                    }
                    GlowStyle::Beam => {
                        // Stardust is weightless: no gravity or arcs. Motes are
                        // born in the rod's wake (along the leap on a jump,
                        // slightly behind the head when typing) and drift
                        // slowly; drawn as twinkling 4-point stars.
                        let (bx, by) = if dist >= 2.0 {
                            // Window-absolute like `ox`/`oy`, so motes follow the
                            // real leap; adds 0.0 at origin (0,0).
                            let (fx, fy) = geom.cell_center(pr, pc);
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
                        // Debris glitter: grains of shed ice that hang (near-zero
                        // gravity), drift against the motion and twinkle as they
                        // disperse. On a jump the grains are strewn along the
                        // swept vector. Window-absolute like `ox`/`oy`, so the
                        // drift vector below trails the real motion (adds 0.0 at
                        // origin (0,0)).
                        let (fx, fy) = geom.cell_center(pr, pc);
                        let (bx, by) = if dist >= 2.0 {
                            // r0 walks the leap vector; grains sit ON the tail.
                            (fx + (ox - fx) * r0, fy + (oy - fy) * r0)
                        } else {
                            (ox + (r0 - 0.5) * geom.cw as f32, oy)
                        };
                        // Drift BEHIND the motion: the shed dust falls back along
                        // the tail. A stationary re-key (no vector) disperses on a
                        // random angle instead.
                        let (mvx, mvy) = (ox - fx, oy - fy);
                        let ml = (mvx * mvx + mvy * mvy).sqrt();
                        let (tx, ty) = if ml > 0.5 {
                            (-mvx / ml, -mvy / ml)
                        } else {
                            let a = r0 * std::f32::consts::TAU;
                            (a.cos(), a.sin())
                        };
                        let speed = (0.25 + r1 * 0.55) * cell;
                        Particle {
                            x0: bx,
                            y0: by,
                            vx: tx * speed + (r2 - 0.5) * 0.4 * cell,
                            vy: ty * speed + (r5 - 0.5) * 0.35 * cell,
                            gy: 0.25 * cell, // dust settles, it doesn't plummet
                            life: 0.45 + r3 * 0.6,
                            hue: r4,
                            cov_scale: 1.0,
                            born: now,
                        }
                    }
                    GlowStyle::Sparkle => {
                        // The celestial pour: stars, moons and mini-comets spill
                        // from the ribbon. Same spread-birth rule as the default
                        // arm (r2 walks the leap or jitters the cell, r4 scatters
                        // off the centre line), with a gentler toss and longer
                        // life. `hue` doubles as the shape seed in
                        // `emit_particles`.
                        let a = r0 * std::f32::consts::TAU;
                        let speed = (0.35 + r1 * 1.3) * cell;
                        let (bx, by) = if dist >= 2.0 {
                            let (fx, fy) = geom.cell_center(pr, pc);
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
                            vy: a.sin() * speed - 0.15 * cell, // a touch of loft
                            gy: 1.1 * cell,                    // constellations drift, not plummet
                            life: 0.5 + r3 * 0.7,
                            hue: (hue0 + r5 * 0.3).fract(),
                            cov_scale: 1.0,
                            born: now,
                        }
                    }
                    _ => {
                        // Sparkles fly outward and fall, rainbow-hued. Births are
                        // spread, never point-stacked (a stacked burst adds into a
                        // white blob): `r2` jitters across the cell or walks the
                        // leap vector on a jump, `r4` scatters off the centre
                        // line. Both are drawn for every arm, so using them here
                        // shifts no RNG stream.
                        let a = r0 * std::f32::consts::TAU;
                        let speed = (0.4 + r1 * 1.6) * cell;
                        let (bx, by) = if dist >= 2.0 {
                            let (fx, fy) = geom.cell_center(pr, pc);
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

    /// Forge one lightning bolt from `from` to `to`: midpoint-displacement
    /// jagging (amplitude halving each round) frozen at spawn, so the strike
    /// holds its shape. A `big` (jump) strike also throws 2–4 thinner branch
    /// forks off interior kinks, each jagged once. Deterministic via the
    /// resident xorshift rng.
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
        // The two reusable jag buffers, taken out of `self` because the loop
        // below calls `self.frand()`; both are handed back before returning.
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
                // Midpoint kicked along the unit perpendicular.
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
            let forks = 2 + (self.frand() * 3.0) as usize; // 2..=4
            for _ in 0..forks {
                // Root the fork at an interior kink; continue the local channel
                // direction bent outward by up to ~55°, at 14–36% of the strike
                // length, with one jag round of its own.
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
                    life: 0.16 + self.frand() * 0.10,
                    seed: self.frand(),
                    branch: true,
                };
                self.push_bolt(bolt);
            }
        }
        let life = if big {
            0.30 + self.frand() * 0.14
        } else {
            0.14 + self.frand() * 0.08
        };
        let seed = self.frand();
        self.push_bolt(Bolt {
            // The one buffer a `Bolt` owns; the jag's scratches return to the
            // pool on the next line.
            pts: pts.clone(),
            born: now,
            life,
            seed,
            // Crackle arcs (small strikes off the typing head) render thin.
            branch: !big,
        });
        self.bolt_jag = (pts, next);
    }

    /// Append a bolt, evicting the oldest once [`Self::MAX_BOLTS`] are live.
    pub(super) fn push_bolt(&mut self, b: Bolt) {
        if self.bolts.len() >= Self::MAX_BOLTS {
            self.bolts.remove(0);
        }
        self.bolts.push(b);
    }

    /// The meteor's debris: at the strike, embers erupt along the coalesced
    /// flight path (biased toward the landing) and die fast, so the landing
    /// row is quiet again well under a second.
    pub(super) fn meteor_strike_fountain(&mut self, m: &Meteor, now: Instant, geom: Geom) {
        let cell = geom.ch as f32;
        let (dx, dy) = (m.x1 - m.x0, m.y1 - m.y0);
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        let dist_cells = len / geom.cw.max(1) as f32;
        // A few sparks shed along the flight direction with a small spread, so
        // the wake reads as a needle shedding sparks.
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
            // sqrt-skewed births: debris concentrates toward the impact.
            let t = r0.sqrt();
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
        // Expire a stale hint: it keeps the animation timer armed
        // (`has_live_motion`), so an unconsumed one must self-disarm.
        if self
            .kill_hint
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() > Self::KILL_HINT_FRESH)
        {
            self.kill_hint = None;
        }
        // The plain-Backspace poof hint self-disarms on the same freshness
        // window as the kill hint (an unconsumed hint would keep the animation
        // timer armed — see `has_live_motion`).
        if self
            .bs_poof_hint
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() > Self::KILL_HINT_FRESH)
        {
            self.bs_poof_hint = None;
            self.bs_baseline = None;
        }
        // A poof is licensed by a kill chord (`kill_hint`) or a plain Backspace
        // (`bs_poof_hint`); both share the span/fallback machinery and the
        // `POOF_MIN_GAP` rate gate. The cue belongs to the kill chord alone: a
        // plain Backspace already spoke on its cursor retreat, so its poof is
        // visual-only and one key never clicks twice.
        let kill_chord = self.kill_hint.is_some_and(|t| {
            now.saturating_duration_since(t).as_secs_f32() <= Self::KILL_HINT_FRESH
        });
        let fresh_kill = kill_chord
            || self.bs_poof_hint.is_some_and(|t| {
                now.saturating_duration_since(t).as_secs_f32() <= Self::KILL_HINT_FRESH
            });
        let gap_ok = self
            .last_poof
            .is_none_or(|t| now.saturating_duration_since(t).as_secs_f32() >= Self::POOF_MIN_GAP);
        // Which key licensed this poof: picks the poof's voice ([`PoofVoice`])
        // and gates the caret fallback's erasure witness. Read before the span
        // branch, which consumes `kill_hint`.
        let bs_only = self.kill_hint.is_none();
        let voice = if bs_only {
            PoofVoice::Puff
        } else if self.kill_hint_word {
            PoofVoice::WordPoof
        } else {
            PoofVoice::Swoosh
        };
        // `poofed` tracks whether the precise span branch answered this frame;
        // the caret-anchored fallback below covers everything it cannot.
        let mut poofed = false;
        if fresh_kill
            && gap_ok
            && let (Some(prev), Some(cur)) = (self.row_prev_meta, self.row_cur_meta)
            // A ContentOnly probe (plain alt screen) carries no kill license:
            // Ctrl-U there is a page scroll (see [`ProbeTrust`]). Both halves of
            // the diff must be full-trust.
            && prev.trust == ProbeTrust::Full
            && cur.trust == ProbeTrust::Full
            && prev.row == cur.row
            && cur.fill < prev.fill
            && now.saturating_duration_since(prev.at).as_secs_f32() <= Self::POOF_PROBE_STALE
        {
            let cur_fill = cur.fill as usize;
            let prev_fill = prev.fill as usize;
            // Common PREFIX, capped at the survivor's fill: past it the new row
            // is blanks, and blanks matching old blanks prove nothing.
            let p = self
                .row_prev
                .iter()
                .zip(self.row_cur.iter())
                .take(cur_fill)
                .take_while(|(a, b)| a == b)
                .count();
            // Common suffix aligned at the two fills (survivors of a Ctrl-U or
            // word kill shift left), capped at the survivor's fill.
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
            // Stable survivors: everything on the row is accounted for by the
            // unmoved prefix plus the shifted suffix (repeated chars may double
            // count). The test is vacuous for a blank new row, and a page scroll
            // can slide in a row sharing an accidental prefix, so a non-trivial
            // survivor is also required below.
            if cur_fill > 0 && p + s >= cur_fill {
                // The vanished span: prefix end .. old fill minus the survivors
                // that shifted in from the right. `s_span` re-caps the suffix so
                // overlap cannot push `c1` left of `c0`, and the previous caret
                // refines `c0` for forward kills inside repeated text.
                let s_span = s.min(cur_fill - p.min(cur_fill));
                let c0 = p.min(prev.caret as usize).min(cur_fill) as u16;
                let c1 = (prev_fill - s_span).max(c0 as usize + 1) as u16;
                // Non-trivial survivor: at least one non-blank cell must survive
                // outside the span (inked prefix left of `c0`, or a shifted
                // suffix). A real kill always leaves such a remnant; a page-scroll
                // replacement or blank scroll-in does not, and stays dark. A
                // refusal falls through: licences keep their freshness windows
                // and the caret fallback below may answer.
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
        // Caret-anchored fallback: every kill keypress earns one poof. The span
        // branch covers a row that shrank in place (a shell's Ctrl-K/EL); a TUI
        // that reflows on a kill defeats any row diff. The hint already proves a
        // kill key was pressed, so answer at the caret with a modest fixed puff.
        // Requires a probe this frame and respects the rate gate. The kill
        // chord's `POOF_FALLBACK_GRACE` gives the span branch first chance.
        let poof_hint_at = match (self.kill_hint, self.bs_poof_hint) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        // A plain Backspace must prove it erased something before the fallback
        // answers (the kill-chord fallback stays permissive). A Backspace at
        // column 0 or on an empty row erases nothing.
        //
        // Two witnesses: `erasure_proven` is the live frame-to-frame diff,
        // which does not exist after a pause; `bs_erased` compares against the
        // row stamped at the key ([`Self::bs_baseline`]).
        let erasure_proven = self
            .row_prev_meta
            .zip(self.row_cur_meta)
            .is_some_and(|(prev, cur)| prev.row == cur.row && cur.fill < prev.fill);
        // The same question against the row at the key ([`Self::bs_baseline`]).
        // With no baseline the host was not composing, so fall back to the
        // weaker witness: the caret is off column 0, so the Backspace could
        // have erased. A no-op Backspace at the left margin stays silent.
        let erase_on_glass = self.poof_erase_witnessed();
        let bs_erased = erase_on_glass
            || matches!(
                (self.bs_baseline, self.row_cur_meta),
                (None, Some(cur)) if cur.caret > 0
            );
        // Skip the rest of the grace when the erase is already on glass: the
        // span branch ran on this same probe and declined, so waiting only adds
        // latency. It cannot steal the span branch's chance: a later match would
        // need a further shrink, i.e. a second erase, which `POOF_MIN_GAP`
        // governs. The timed grace remains the cap when there is no witness (a
        // reflow that re-rowed the caret, an echo that never shows, no probe at
        // the key).
        if fresh_kill
            && gap_ok
            && !poofed
            && (!bs_only || erasure_proven || bs_erased)
            && (erase_on_glass
                || poof_hint_at.is_some_and(|t| {
                    now.saturating_duration_since(t).as_secs_f32() >= Self::POOF_FALLBACK_GRACE
                }))
            && let Some(cur) = self.row_cur_meta
            // Same trust law as the span branch: a plain-alt ContentOnly probe
            // cannot anchor the caret fallback either (see [`ProbeTrust`]).
            && cur.trust == ProbeTrust::Full
        {
            let c0 = cur.caret;
            self.spawn_poof(cur.row, c0, c0 + 3, 3, now, cfg, geom, voice);
            self.last_poof = Some(now);
            self.kill_hint = None;
            self.bs_poof_hint = None;
            self.bs_baseline = None;
        }
        // Rotate only when a fresh probe arrived this frame; an unprobed frame
        // keeps the previous truth. The content witnesses' neighbour captures
        // rotate with it, so metadata and buffers stay one unit.
        if self.row_cur_meta.is_some() {
            std::mem::swap(&mut self.row_prev, &mut self.row_cur);
            std::mem::swap(&mut self.row_above_prev, &mut self.row_above_cur);
            std::mem::swap(&mut self.row_below_prev, &mut self.row_below_cur);
            self.row_prev_meta = self.row_cur_meta.take();
        }
    }

    /// Emit one erase poof over the vanished span `[c0..c1)` of `row` (`n` =
    /// net columns erased) in the style's own language (additive light or
    /// source-over veils, never squares over glyphs):
    /// - default (Lumen/Laser/Beam/Phaser): a little smoke poof, neutral grey
    ///   [`VaporKind::Poof`] puffs with buoyant motion and a short life;
    /// - fire: quench steam (the deletion-echo burst params), while
    ///   [`Self::note_kill`]'s quench damps the flame;
    /// - rainbow kitty and sparkle: star-power sparkles strewn across the span;
    /// - water: droplets spraying up and falling out of the span;
    /// - comet: ice glints.
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
        // Sound: the poof's own noise, under the same `POOF_MIN_GAP` rate limit
        // as the light. The voice depends on the licensing key:
        //
        // * a line kill (^U/^K) has no keystroke of its own, so the downward
        //   [`SoundKind::Kill`] swoosh is its whole voice;
        // * a word kill (^W, Alt-D, Alt/Ctrl-Backspace) gets
        //   [`SoundKind::KillWord`], one size up from the erase poof;
        // * a plain Backspace already spoke on its cursor retreat, so it gets
        //   the quiet [`SoundKind::Poof`] air under that voice.
        match voice {
            PoofVoice::Swoosh => self.cue_sound(crate::trail_sound::SoundKind::Kill, c0),
            PoofVoice::WordPoof => self.cue_sound(crate::trail_sound::SoundKind::KillWord, c0),
            PoofVoice::Puff => self.cue_sound(crate::trail_sound::SoundKind::Poof, c0),
        }
        // A kill reaches v2 from here, the erase detector, which holds both the
        // proven span (`n`) and the licence's scale (`voice`), with the caret
        // where the kill left it. A plain Backspace mints nothing (its `Erase`
        // went out at the key). A caret-moving kill whose `Kill` went out with
        // its retreat (`kill_reported_to_v2`) is not minted twice; otherwise it
        // is minted here once, keyed on the kill hint's stamp. The hint is still
        // armed here; callers clear it after.
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
        // Puff x-centers spread across the span (k+0.5 of `puffs` slots).
        let nf = n as f32;
        // The particle-only styles (rainbow kitty, sparkle, water, comet) also
        // shed a few neutral grey puffs on every theme: on light grounds via
        // `push_halo_over`'s veil path, on dark via the renderer's dark
        // `VaporKind::Poof` arm. A delete then reads as removal (grey, buoyant,
        // dispersing) rather than the rising glitter that means creation
        // (owner, 2026-08-29).
        if matches!(
            cfg.style,
            GlowStyle::RainbowKitty | GlowStyle::Sparkle | GlowStyle::Water | GlowStyle::Comet
        ) {
            self.spawn_poof_smoke(span_x0, span_w, oy, cell, cwf, n, now);
        }
        match cfg.style {
            GlowStyle::RainbowKitty | GlowStyle::Sparkle => {
                // Under v2 the erase's stars are stardust's; only the smoke above
                // is v1-owned, so the glitter cloud lays nothing while v2 owns
                // the frame.
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
                // Water: droplets spray up out of the span and fall; comet: the
                // same ballistics read as ice glints through its twinkle ramp,
                // settling instead of plummeting.
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
                // Quench steam (the deletion-echo burst), scaled by the span and
                // spread across it, while note_kill's quench damps the blaze.
                let puffs = ((1.0 + nf.sqrt()) as usize).clamp(1, 5);
                let x_at = |k: usize| span_x0 + ((k as f32 + 0.5) / puffs as f32) * span_w;
                self.spawn_quench_steam(puffs, x_at, oy, cell, cwf, now);
            }
            _ => {
                // The default little smoke poof (Lumen/Laser/Beam/Phaser).
                self.spawn_poof_smoke(span_x0, span_w, oy, cell, cwf, n, now);
            }
        }
    }

    /// The erase sparkle poof, shared by both entry points: the span detector
    /// ([`Self::spawn_poof`], exact erased range) and the deletion echo in
    /// [`Self::spawn`] (one vacated cell as the caret steps left). The rainbow
    /// kitty / sparkle language for "text went away": glitter twinkles off the
    /// vanished span plus one bright white-cored pop at its centre.
    ///
    /// Energy is [`erase_poof_drive`] (momentum × weight), setting star count,
    /// launch speed and life. Erasing walks the caret left, so the debris
    /// streams right, mirroring the typing plume.
    ///
    /// Bounded by [`ERASE_POOF_CAP`] and the shared `MAX_PARTICLES` ceiling.
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
        // Debris rises, except on the first grid row, where it would be thrown
        // past the viewport top; flip the bias downward when the row cannot
        // afford the lift.
        let lift = if oy - top < cell * ERASE_POOF_LIFT_CELLS {
            -1.0
        } else {
            1.0
        };
        let stars = ((ERASE_POOF_STARS * drive) as usize).clamp(4, ERASE_POOF_CAP);
        // Speed rides the RUN, size rides the SPAN: a fast correction flicks,
        // a killed line heaves.
        let speed = 0.55 + 0.85 * mom.clamp(0.0, 1.0);
        let heft = erase_weight(n) / ERASE_WEIGHT_CAP; // 0..1
        // Debris clusters at the collapse point ([`ERASE_POOF_SPREAD_CELLS`]):
        // `f²` biases toward the span's start and the width is capped.
        let spread_w = span_w.min(cwf * ERASE_POOF_SPREAD_CELLS);
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
                // A heavier erase has more VOLUME, not just more grains.
                y0: oy + (r1 - 0.5) * cell * (0.6 + 0.9 * heft),
                // RIGHTWARD — trailing the leftward-walking caret (see the doc).
                vx: (0.35 + r2 * 1.25) * cell * speed,
                vy: -lift * (0.3 + r3 * 1.1) * cell * speed,
                // Gravity is down whichever way the debris was thrown, so a
                // downward poof does not accelerate back up off the viewport.
                gy: 0.9 * cell,
                life: (0.42 + r4 * 0.55) * (0.85 + 0.45 * heft),
                hue: r0, // twinkle seed
                // Round-spark sentinel ([`ERASE_ROUND_SCALE`], inside the glitter
                // band `< 0.9`): a deletion keeps full brightness regardless of
                // typing momentum (the glitter arm pins `mom = 1.0`), and the body
                // draws round; the hero below is the poof's one plus.
                cov_scale: ERASE_ROUND_SCALE,
                born: now,
            });
        }
        // One bright white-cored pop at the erased span so even a lone
        // Backspace reads: near-stationary, short-lived, `hue < 0.4` for the
        // white core, momentum-independent, lingering longer for a line kill.
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
                // `< 0.4` selects the white core on dark; on light the seed
                // selects a band, and 0.02 lands on the family's red, which reads
                // as a spark on both grounds.
                hue: 0.02,
                // The hero: in the reserved top of the glitter band,
                // `emit_particles` reads 0.80..=0.89 as "draw big", scaled by the
                // heft in the fraction, so the poof shows how much vanished.
                cov_scale: ERASE_HERO_SCALE_MIN + ERASE_HERO_SCALE_SPAN * heft,
                born: now,
            });
        }
    }

    /// The neutral-grey smoke poof puffs: the hot-cursor smoke's buoyant motion
    /// with a short life. The poof's body in every style without its own: the
    /// whole poof for the default styles, and the billow the rainbow-kitty /
    /// sparkle glitter rides on. Puffs cluster at the collapse point by the same
    /// rule as the sparkles ([`ERASE_POOF_SPREAD_CELLS`], `f²` bias).
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
        // Weight, as for the sparkles: a killed line billows where a single
        // character wisps.
        let puffs = ((1.0 + f32::from(n).sqrt() * 1.4) as usize).clamp(1, 8);
        // Clustered: the spread is capped and `f²` biases births toward the
        // span's start, so the puffs overlap into a billow where the line
        // collapsed rather than thin wisps spread along it.
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
                // Fast enough that the cloud visibly lifts and disperses as it
                // thins; still buoyant and short.
                vx: (r1 - 0.5) * 0.55 * cell,
                vy: -(0.55 + 0.45 * r2) * cell,
                gy: -0.10 * cell, // buoyant, like the cursor smoke
                life: 0.6 + 0.5 * r0,
                seed: r1,
                kind: VaporKind::Poof,
                born: now,
            });
        }
    }

    /// Quench steam, the one burst behind a deletion echo
    /// ([`Self::update_typing_thermals`]) and a killed span
    /// ([`Self::spawn_poof`]): `puffs` billowing puffs born at `x_at(k)` (±
    /// half a cell `cw`) on `oy`, every third a tiny bright short-lived hiss
    /// speck. Expands fast, stalls under drag, dies young; bounded by
    /// `MAX_VAPOR`.
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
                vy: -(0.9 + 0.7 * r2) * cell * (if hiss { 1.6 } else { 1.0 }),
                gy: 1.4 * cell, // drag: the rise stalls (steam disperses)
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
