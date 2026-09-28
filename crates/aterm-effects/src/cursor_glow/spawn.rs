// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SPAWNING — the light, particles, rings, bolts and poofs a licensed move
//! or an erase mints into the engine's state.

use super::*;

// `GlideStar` STOOD HERE and is deleted (step 13). It was a whole transient
// type — endpoints, life, a velocity for its head brightness, a seed for its own
// hue rotation — for a mark that was the ZOOM streak fired on a different
// trigger. A fast glide pushes a [`JumpStreak`] now, so a mark whose only
// difference from another mark was WHICH FUNCTION DREW IT no longer has a type
// to be different in.

// ---- THE ERASE POOF's ENERGY -----------------------------------------------
//
// Owner, 2026-08-06: bring the delete poof back "with a similar momentum as
// forward typing and also include some weight for the size of what is deleted".
// Two independent terms, multiplied:
//
//   * MOMENTUM — how hard the delete RUN is going, read from
//     [`CursorGlow::erase_mom`], the mirror of the canonical typing metric.
//   * WEIGHT — how much VANISHED, in cells.
//
// The old poof had neither: a fixed 4..10 stars whose count crept up by one per
// two erased columns and saturated at ten, so a lone Backspace and a killed
// 80-column line threw indistinguishable puffs, and a held delete run looked
// exactly like a single correction.
/// Floor of the momentum term. Deliberately high: a delete you make ONCE, cold,
/// must still be clearly acknowledged — the poof is feedback before it is
/// choreography. Momentum then carries it the rest of the way.
pub(super) const ERASE_MOM_FLOOR: f32 = 0.55;
/// Ceiling on the WEIGHT term. `sqrt(cells)` grows the burst with what vanished
/// (1 cell → 1.0, a six-letter word → 2.4, a killed line → this cap) without
/// letting a full-line kill scale linearly into a screen-filling cloud.
pub(super) const ERASE_WEIGHT_CAP: f32 = 3.4;
/// Sparkles at drive 1.0. Sized so a COLD single-character Backspace (drive =
/// [`ERASE_MOM_FLOOR`]) still throws the handful the shipping poof always did
/// — the energy law must make deletes MORE expressive, never quietly thin out
/// the most common one — while a hot word kill lands mid-band and a hot line
/// kill hits [`ERASE_POOF_CAP`].
///
/// 7 → 11 (sized so a line kill would look heavy; it looked like a cloud) → 7
/// in the blind density cut → 8. The cut restored the pre-inflation number
/// exactly, which is the tell that it was undoing rather than tuning: a
/// dark-ground capture of Ctrl-W at 7 puts about half a dozen grains around the
/// caret, which reads as a small puff rather than as something vanishing. One
/// more grain, and the taper makes each of them a lighter mark than the crosses
/// the old number was judged against.
pub(super) const ERASE_POOF_STARS: f32 = 8.0;
/// Hard ceiling on one poof's sparkle count, well under `MAX_PARTICLES` so a
/// held Ctrl-U mash can never crowd out the rest of the family. Moved with
/// [`ERASE_POOF_STARS`] so a full line kill keeps the same headroom over the
/// word kill it has to out-weigh.
pub(super) const ERASE_POOF_CAP: usize = 18;
/// How wide, in CELLS, one poof's debris may spread — regardless of how long
/// the erased span was.
///
/// A killed 60-column line spread evenly over its own span is a DOTTED RULE
/// across the window, which is what a white-ground capture showed: sixty pale
/// specks in a neat row, reading as a horizontal line rather than as something
/// vanishing. The span still sets the poof's ENERGY (via [`erase_weight`]); it
/// no longer sets its geometry. Debris clusters at the collapse point — the
/// span's start, where the caret ends up — and thins outward.
pub(super) const ERASE_POOF_SPREAD_CELLS: f32 = 9.0;
/// How much headroom above the erased row the debris needs before it is allowed
/// to rise. Inside this, the poof falls instead — see `spawn_erase_sparkles`.
pub(super) const ERASE_POOF_LIFT_CELLS: f32 = 1.2;
/// The HERO grain's `cov_scale` band, reserved at the top of the GLITTER
/// sentinel range (`< 0.9`): `MIN` at a single character, `MIN + SPAN` at a
/// killed line. `emit_particles` decodes it back into a size multiplier, so the
/// poof's central pop grows with the weight of what vanished while every other
/// grain keeps the plain glitter size.
pub(super) const ERASE_HERO_SCALE_MIN: f32 = 0.80;
pub(super) const ERASE_HERO_SCALE_SPAN: f32 = 0.09;
/// The ROUND-SPARK band, at the bottom of the GLITTER sentinel range: the
/// poof's BODY grains spawn at [`ERASE_ROUND_SCALE`], and `emit_particles`
/// reads anything under [`ERASE_ROUND_SCALE_MAX`] as "draw ROUND" — the
/// old-style spark, a radial point of cool light — instead of the 4-point
/// plus. Owner: "the extra '+' sparkles on cursor are too extreme. I like the
/// older style of sparks … but a few of the '+' are nice." The all-plus poof
/// was worse than an aesthetic miss: a cloud of small crosses at glyph size
/// reads as literal '+' characters typed into the buffer, and a terminal must
/// never appear to emit glyphs it didn't. The poof keeps exactly ONE plus —
/// the hero band above — so the accent survives while the body goes back to
/// sparks.
///
/// STALE CLAIM RETIRED: this doc used to end "the middle of the glitter band
/// (the trail's own star and the terminus melt-out, both 0.7) still draws the
/// plus twinkle, unchanged". It does not, and has not since THE STARDUST LAW
/// ([`star_accent`]): the 0.7 band now falls through this sentinel to the
/// population deal in `emit_particles`, which draws round [`dust_r`] stardust
/// unless the grain's stored seed is dealt the
/// 1-in-[`crate::effect_util::STAR_ACCENT_DEN`]
/// accent. The `< ERASE_ROUND_SCALE_MAX` sentinel is now the narrower
/// statement it reads as — "this grain is poof BODY, round unconditionally"
/// — and the hero band is the only thing that still buys a plus outright.
pub(super) const ERASE_ROUND_SCALE: f32 = 0.5;

/// The WEIGHT term: how big the vanished span reads, in
/// `1.0..=`[`ERASE_WEIGHT_CAP`]. Monotone non-decreasing in `cells` (pinned by
/// `erase_poof_drive_grows_with_momentum_and_weight`).
#[inline]
pub(super) fn erase_weight(cells: u16) -> f32 {
    f32::from(cells.max(1)).sqrt().min(ERASE_WEIGHT_CAP)
}

/// The poof's ENERGY: the momentum term (floored at [`ERASE_MOM_FLOOR`]) times
/// the [`erase_weight`] of what vanished. Monotone non-decreasing in BOTH
/// arguments, which is the whole contract — a faster run poofs harder, and a
/// bigger erase poofs harder, independently.
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
        // SOUND: record the aural twin of this spawn, with the classification
        // the light already computed (after the heat/flare update, so the
        // cue's blaze matches what this frame will look like). Kill cues fire
        // at the poof site instead — a kill's echo may not move the caret.
        //
        // An admitted navigation-classified move sings in the melody. A short
        // scrub GLIDES one in-key step; a fast/coalesced run SWEEPS a short scale
        // run, both carrying travel direction. Raw key timestamps never reach
        // this cue path because sound shares the visual candidate gate.
        //
        // ONE KEYPRESS, ONE CLICK — DEBITED ONLY BY THE ARMS A PRINTABLE KEY'S ECHO
        // CAN LAND ON (`typing` and the `else` Jump), never by `deletion` or
        // `navigation`.
        //
        // Retiring the credit ahead of the chain looks more exhaustive and is
        // WRONG, measurably. Those two arms are hint-only: `deletion` is
        // direction-blind (a forward +1 move satisfies it) and `navigation` tests no
        // shape at all, so the dominant case is a printable key's echo being
        // OVERTAKEN by the next key's hint and misclassifying into one of them. The
        // printable echo then still surfaces — and with the credit already retired
        // it falls through to `typing` with an empty ledger and cues a spurious
        // LETTER click on a Backspace or an arrow. Measured: `x` then Backspace
        // yields [Typed, Backspace, Typed] — three sounds for two physical keys —
        // where debiting inside the arms yields [Typed, Backspace].
        //
        // The orphan this trades away is real but strictly milder: a swallowed
        // printable echo can leave a credit that mutes one later genuine click,
        // and it self-clears at `KEYED_CLICK_FRESH`. A wrong sound now beats a
        // missing sound later.
        if deletion {
            self.cue_sound(crate::trail_sound::SoundKind::Backspace, cc);
        } else if navigation {
            // SEAM POINT 11 (§17.2, §12.3): while v2 owns the frame the
            // meteor gesture — or the nav tick — is the ONE sound of a
            // navigation move, minted by the engine on the observed frame;
            // v1's Sweep / Glide never speak beside it. Nested rather than
            // folded into the arm's test, so a one-cell nav move cannot fall
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
            // ONE CHARACTER, ONE CLICK. If the host already clicked this glyph
            // at the physical keypress ([`Self::cue_keystroke`]), spend that
            // credit and stay silent — otherwise every keystroke would speak
            // twice, once at the key and again an echo round trip later, which
            // reads as a stutter rather than as speed. An advance with NO
            // credit behind it (pure PTY output, a host that never arms the key
            // seam, an echo that outran the freshness window) still clicks here
            // byte-identically to before the seam existed.
            if !self.take_keyed_click(now) {
                self.cue_sound(crate::trail_sound::SoundKind::Typed, cc);
            }
        } else {
            // A printable keystroke does NOT always echo as `typing`: whenever its
            // echo moves the cursor more than one cell — vim normal-mode motions
            // (`w b e 0 $ G } %`, `f<char>`), any printable key in less/htop/fzf, a
            // multi-cell IME commit — it classifies here as a Jump. The key-time
            // credit must therefore be spent on THIS arm too, or one keypress speaks
            // twice: `Typed` at the key plus `Jump` at the echo. The synth cannot
            // thin that pair, because `Jump` is on the MIN_GAP bypass list, so the
            // double is audible even at 1 ms separation.
            //
            // The leak is the worse half: an unspent credit survives
            // `KEYED_CLICK_FRESH` and is then taken by the NEXT typing spawn — which
            // may have no key behind it at all — so one `w` in vim bought a spurious
            // click AND muted a real one.
            //
            // The KEY click wins: it already reached the ear at the physical press,
            // which is the entire point of the seam. A Jump with no credit behind it
            // (pure PTY output, a program repositioning the cursor) still cues here
            // byte-identically to before the seam existed.
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
                // the rainbow kitty's ribbon must read as a rainbow even at a stroll, but its
                // DYNAMIC RANGE is wide: a CALM cold floor (a stroll lays a quiet
                // trail) that climbs steeply with momentum (a hot run blazes),
                // still under the per-band legibility ceiling that guards text in
                // `emit_rainbow`.
                // Rides the EASED spine, so a new spark's brightness swells with
                // momentum instead of stepping to full on the first fast key —
                // floored at the decaying peak memory so a resumed key opens at
                // the band it re-joins ([`Self::rainbow_birth_disp`]).
                // Phaser: CONSTANT brightness at any cadence — the band must
                // look identical fast or slow (its constant-distance promise),
                // so it skips the heat ramp entirely.
                GlowStyle::Phaser => 1.0,
                // Fire brightens on the EASED display temperature: the coal
                // floor keeps human-cadence typing visibly alight, and the
                // ramp swells instead of popping.
                GlowStyle::Fire => 0.28 + 0.92 * self.fire_t(),
                // Beam: steady light has exactly one brightness — the tube must
                // not fade in as the heat builds, or the first keys of a run
                // read as a dim smear.
                GlowStyle::Beam => 1.0,
                // Trail Pack: the birth-brightness ramp is the pack's own
                // `heat.bright_floor + bright_slope·heat` (mirrors the shared
                // `typing_boost` 0.28+0.92·heat shape but from data). Reached ONLY
                // for Custom, so every built-in arm above stays byte-identical.
                GlowStyle::Custom => cfg.pack.as_ref().map_or_else(
                    || self.typing_boost(),
                    |p| p.heat.bright_floor + p.heat.bright_slope * self.heat,
                ),
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
        // Typing → a short, tight wake (~100 ms) so the cursor reads as LEADING and
        // crisp; a real jump → the full comet duration. Per-spark lifetime, decoupling
        // the typing wake from the jump comet so users can keep a long, dramatic jump
        // trail without their typing smearing. Heat stretches the typing wake (up to
        // ~2.3×) so a fast run leaves a visible streak — the acceleration tell.
        // TYPING CONTINUITY: during a rhythm (inter-key gap ≤ CHAIN_GAP_MAX) the
        // spark's life additionally CHAINS to the observed cadence (gap + margin,
        // capped) so the previous wake is still alive when the next key lands — a
        // continuous streak at any human typing speed instead of per-key pulses
        // with dark rests. The FIRST key of a burst (gap = ∞) takes no floor, so a
        // lone keystroke fades exactly as crisply as before.
        let full_life = cfg.duration.as_secs_f32().max(0.001);
        if typing {
            match cfg.style {
                // Phaser: a CONSTANT-DISTANCE streak — the fat spectrum band
                // always spans the last two typed cells FULLY LIT, however fast
                // or slow the typing. Life is [`Self::PHASER_CHAIN_KEYS`]×
                // the observed inter-key gap (+ fade margin) — cadence-scaled,
                // not a fixed time and not heat-stretched, so a fast run and a
                // hunt-and-peck leave the SAME two-letter band. Only the first
                // key of a burst (no observed cadence yet) takes the base time
                // instead, and the window/cap are generous so even leisurely
                // rhythms chain.
                GlowStyle::Phaser => {
                    if gap <= Self::PHASER_CHAIN_GAP_MAX {
                        (Self::PHASER_CHAIN_KEYS * gap + Self::CHAIN_MARGIN)
                            .min(Self::PHASER_CHAIN_LIFE_MAX)
                    } else {
                        (full_life * 2.0).clamp(0.40, 0.75)
                    }
                }
                // Water at speed reads as a TORPEDO: its wake stretches up to ~4×
                // — the deep ocean palette needs a long visible swell to read as
                // a TRAIL of water rolling behind the cursor, not a stub.
                GlowStyle::Water => (full_life * 0.42).clamp(0.06, 0.14) * (1.0 + 3.0 * self.heat),
                // Fire BURNS ON: embers don't vanish, they die down — the wake
                // leaves a real TRAIL OF FIRE behind the cursor. The ceiling honours a long configured
                // `cursor_trail_ms` (up to ~0.45s base, ~1.4s at full heat)
                // while the curtain's early-fade envelope keeps the old tail
                // ember-dim, never a bright smear. Chained to cadence like the
                // default arm.
                GlowStyle::Fire => {
                    // Life stretches on the eased temperature (coal-floored):
                    // at human cadence the bed keeps embers alive well past
                    // one inter-key gap — the persistent ember line.
                    let heat_life =
                        (full_life * 0.38).clamp(0.10, 0.60) * (1.0 + 2.5 * self.fire_t());
                    if gap <= Self::CHAIN_GAP_MAX {
                        heat_life.max((gap + Self::CHAIN_MARGIN).min(Self::CHAIN_LIFE_MAX))
                    } else {
                        heat_life
                    }
                }
                // BEAM: a STEADY tube — its typed span is cadence-scaled like
                // the phaser's constant-distance band ([`Self::BEAM_CHAIN_KEYS`]
                // inter-key gaps), so the rod spans about the last 3.5 letters
                // at any rhythm, with `beam_power`'s full-power hold keeping the
                // newest letters solidly lit. Only the first key of a burst
                // (gap = ∞) takes the base time, so a lone keystroke still
                // powers down crisply.
                GlowStyle::Beam => {
                    if gap <= Self::BEAM_CHAIN_GAP_MAX {
                        (Self::BEAM_CHAIN_KEYS * gap + Self::CHAIN_MARGIN)
                            .min(Self::BEAM_CHAIN_LIFE_MAX)
                    } else {
                        (full_life * 0.5).clamp(0.13, 0.40)
                    }
                }
                // LIGHTNING stays CHARGED: the typed path holds its charge long
                // after the key — the trail honours a long configured
                // `cursor_trail_ms` (ceiling 0.11s -> 0.40s base, ~1.1s at full
                // heat) while the discharge envelope in `emit_comet` bleeds
                // aged cells down to a dim flickering residual, so the
                // lingering trail reads as static hanging in the air behind
                // the cursor, never a full-power smear over the glyphs.
                // Chained to cadence like the default arm.
                GlowStyle::Laser => {
                    let charge_life =
                        (full_life * 0.30).clamp(0.08, 0.40) * (1.0 + 1.8 * self.heat);
                    if gap <= Self::CHAIN_GAP_MAX {
                        charge_life.max((gap + Self::CHAIN_MARGIN).min(Self::CHAIN_LIFE_MAX))
                    } else {
                        charge_life
                    }
                }
                // Trail Pack: the wake life is the pack's own heat curve
                // (`full_life·life_base_mul · (1 + life_a·h + life_b·h²)`) chained
                // to the observed cadence exactly like the default/phaser arms but
                // from data (`chain_keys·gap + margin`, capped at `chain_life_max`,
                // inside `chain_gap_max`). Clamped to the engine's particle-life
                // ceiling so no param set can park a spark past the cap. Reached
                // ONLY for Custom, so every built-in life arm is byte-identical.
                GlowStyle::Custom => {
                    let h = self.heat;
                    let (hp, chain) = cfg
                        .pack
                        .as_ref()
                        .map(|p| (p.heat, true))
                        .unwrap_or_else(|| (TrailParams::defaults().heat, false));
                    let heat_life = (full_life * hp.life_base_mul)
                        .clamp(0.02, Self::MAX_TRAIL_SPARK_LIFE)
                        * (1.0 + hp.life_a * h + hp.life_b * h * h);
                    let heat_life = heat_life.min(Self::MAX_TRAIL_SPARK_LIFE);
                    if chain && gap <= hp.chain_gap_max {
                        heat_life
                            .max((hp.chain_keys * gap + Self::CHAIN_MARGIN).min(hp.chain_life_max))
                    } else {
                        heat_life
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
            // The phaser JUMP streak is a brisk swoosh: long enough for the
            // eye to register the sweep, gone well under a second — the hard
            // clamp keeps a long configured `cursor_trail_ms` from parking the
            // fat bar over the swept line.
            (full_life * 0.5).clamp(0.30, 0.90)
        } else {
            full_life
        }
    }

    /// Is this spark a RESIDENT — will it still be here once this frame's decay
    /// pass has run? ONE PREDICATE, ASKED AT BOTH ENDS. `tick` SPAWNS BEFORE IT
    /// REAPS, so a producer asking "is this cell already lit?" must ask exactly
    /// the question the reaper is about to ask. A looser question declines to
    /// lay a cell whose only occupant is about to be swept away — a hole in the
    /// mark. A tighter one lays a second spark on an occupant that is staying —
    /// a duplicate, which is the one thing [`Self::emit_rainbow_ribbon`]'s flat
    /// index cannot represent.
    #[inline]
    pub(super) fn spark_resident_at(now: Instant, s: &Spark) -> bool {
        now.saturating_duration_since(s.born).as_secs_f32() < s.life
    }

    /// The per-move LIGHT: exactly one of the style arms — fire meteor, rainbow kitty
    /// jump choreography, the navigation no-wake arm, the rainbow kitty backspace
    /// glitter poof, or the swept comet/ribbon path. Returns how many hue
    /// steps this move advances (a coalesced typing run counts its laid cells;
    /// every other move keeps the classic one step).
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
            // MULTI-HOP COALESCING: shell repaints hop the cursor through
            // parked intermediate positions (CR → col 0 → prompt reprint —
            // several observed moves within a frame or two). A fresh meteor
            // whose ORIGIN is a young live meteor's LANDING retargets it in
            // place: one choreography, one streak. The comparison anchor must
            // convert to window space IDENTICALLY to the landing above, or
            // multi-hop coalescing dies.
            // Search EVERY live meteor (newest first), not just the last one: a
            // hop whose origin matches an OLDER live meteor's landing must
            // retarget THAT meteor, or a stray second streak spawns (with
            // METEOR_CAP up to four choreographies can overlap, and `.last_mut()`
            // could only ever see one of them).
            let (o_x, o_y) = geom.cell_center(pr, pc);
            let retarget = self.fire_meteors.iter_mut().rev().find(|m| {
                now.saturating_duration_since(m.born).as_secs_f32() <= Self::METEOR_RETARGET_S
                    && (m.x1 - o_x).abs() < cwf * 0.6
                    && (m.y1 - o_y).abs() < chf * 0.6
            });
            // METEOR DRAMA: long enough that the flight spans more than the 1-2
            // frames a human jump distance would otherwise give it, and still
            // brisk — the whole show stays under half a second.
            let life = (0.16 + 0.005 * dist).clamp(0.20, 0.45);
            if let Some(m) = retarget {
                m.x1 = x1;
                m.y1 = y1;
                m.life = m.life.max(life);
                // Restart the flight AND the retarget window from THIS hop: a
                // choreography that lags past METEOR_RETARGET_S from the FIRST
                // hop (a prompt reprint can lag ~100 ms on a long edit line)
                // then keeps chaining into ONE streak. Measuring the window from
                // a never-refreshed birth lets the chain break into two streaks.
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
                    // Momentum FLOOR: a cold-start Ctrl-A/E still throws a
                    // BRIGHT streak — the responsive read — instead of a faint
                    // whisper; hot runs are unaffected (fire_t sits above it).
                    mom: self.fire_t().max(0.55),
                    arrived: false,
                });
            }
        } else if navigation && (!water || dist < 2.0) {
        } else {
            let cap = Self::MAX_SPARKS;
            // LASER, BEAM and WATER jumps ignore the configured typing-tail
            // length. Their style-native streak must span the whole observed
            // leap rather than begin in mid-air: lightning/rod for the first
            // two, a fluid crest for Water. The resident cap still bounds the
            // destination-nearest suffix of an outlier vector.
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
                // NON-RAINBOW KITTY WRAP RUN (phaser/fire/…): the trail lives BEHIND the
                // cursor (the block draws the caret itself), so lay the TYPED
                // cells the echo swept through the fold — the last cells of the
                // old row and the glyphs already landed on the new one, EXCLUDING
                // the live caret cell. NOT a Bresenham diagonal back across
                // cells the cursor only skimmed (a smear), and NOT just the empty
                // cursor cell (which leaves the pre-wrap tail and the first
                // wrapped glyphs bare while the older row still blazes — a
                // recency inversion at the fold). Bounded by the wrap
                // SHAPE guards: ≤4 tail + ≤3 head cells.
                let pane_col1 = pane_col0.saturating_add(pane_cols);
                for c in (pc as i32)..(pane_col1 as i32) {
                    self.path_scratch.push((pr as i32, c));
                }
                for c in (pane_col0 as i32)..(cc as i32) {
                    self.path_scratch.push((cr as i32, c));
                }
            } else if re_anchor {
                // TUI RE-ANCHOR (non-rainbow-kitty): the caret never travelled the
                // interpolated cells — the repaint relocated it — so only the
                // landing gets a wake. (A bare-terminal fold that also carries
                // admitted typed intent takes the wrap-run arm above: its cells really
                // were typed.)
                self.path_scratch.push((cr as i32, cc as i32));
            } else {
                // Everything else — a real jump AND a PHASER/non-rainbow-kitty COALESCED
                // ECHO (2-3 glyphs landed in one observed move, `echo_run`
                // above collapsed it to typing) — lays the trail BEHIND the
                // cursor. For a short echo the destination-nearest cells are
                // adjacent, so `line_cells_tail` lays every swept cell gap-free
                // (no picket-fence hole) while excluding the live caret cell;
                // each is a typed head in its own successive hue via
                // `typing_run` below. The echo MUST classify as typing, not a
                // jump: a jump interleaves long-lived jump sparks between the
                // short typing ones and gaps the trail.
                line_cells_tail(
                    &mut self.path_scratch,
                    (pr as i32, pc as i32),
                    (cr as i32, cc as i32),
                    max_len,
                    false,
                );
            }
            // FIRE STAYS ON ONE LINE: a row change (Enter,
            // a wrap, arrow down, a click elsewhere) must leave NO fire on the
            // line you left. The swept path keeps only its landing-row segment
            // (so a same-row leap still burns across the line), and every spark
            // and ember already burning on another row is SNUFFED — its
            // remaining life clamped to a fast die-down, so the old line's
            // flames visibly gutter out in a beat instead of burning on behind
            // you (or vanishing in a hard pop). The jump FLARE + landing ring +
            // ember fountain still announce the leap at the new line.
            // …and a BACKWARD same-row leap gets the same treatment: at the
            // BOTTOM row an Enter scrolls the screen instead of moving the
            // cursor down, so it arrives here as a same-row jump back to
            // column 0 — sweeping backward would streak the fresh prompt line,
            // and the old sparks would hover over scrolled-up text. Ignite at
            // the landing, snuff what's behind. (Forward same-row leaps — tab
            // completion, cursor-right — still sweep the cells they cross.)
            // PHASER shares the whole discipline — the band must follow the
            // typing, never park where you last typed: its trail is the last
            // three letters, so navigation never drags or strands the fat
            // spectrum bar.
            let fire_back_leap = !typing && cc < pc && pr == cr;
            // A TYPING WRAP is exempt for the phaser: the band FOLLOWS the
            // typing through the fold — the pre-wrap tail fades on its own
            // chained life while the new row's head continues the streak.
            // Clearing + snuffing here instead breaks the trail at every wrap
            // (fresh wrapped glyphs unlit, a hard cut on the old row).
            // Fire keeps its one-line discipline even on a wrap (below).
            // A settled RESIZE is exempt too. The
            // rule below exists because a phaser bar swept across a line the
            // cursor merely LANDED on parks there decorating letters nobody
            // typed. A relayout is the one row change where the sweep is the
            // whole point: the cursor's home moved and the band is what shows
            // you where it went. Narrow by construction — it rides the same
            // one-shot license as every other style, so ordinary jumps keep the
            // typed-letters-only discipline exactly as before.
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
                    // Phaser keeps NO landing segment on a row change either:
                    // fire's landing swoosh pays off in flames and embers, but
                    // a phaser bar swept across the fresh line just parks
                    // there — the band belongs to typed letters only.
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
                // A coalesced typing run treats every swept cell as a typed
                // head. Its ancillary hue metadata advances one step per cell,
                // while the visible classic positions are assigned together by
                // the run reflow after spawning.
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
                    // Tail→head grade 0..1 — EXCEPT rainbow typing sweeps/folds AND
                    // non-rainbow-kitty coalesced echo/wrap runs (`typing_run`): a per-key
                    // observer would have laid every one of those cells as its own
                    // single-cell head (pos 1.0), so grading them would burn a
                    // permanently dimmer notch into the ribbon / spectrum bar
                    // between full-brightness per-key neighbours.
                    let pos = if typing_run {
                        1.0
                    } else {
                        (i as f32 + 1.0) / n // tail→head 0..1
                    };
                    // Faint tail → bright head, scaled by the typing heat (jump = 1×);
                    // heat can overdrive past the static curve, so clamp into u8.
                    // LASER: a beam carries near-CONSTANT power along its length —
                    // tail still at ~80% — so a jump reads as one solid ray of
                    // light, not a comet dying out toward its tail, and the head
                    // fires at FULL 255 (a laser has exactly two states: off and
                    // maximum).
                    let born_cov = match cfg.style {
                        GlowStyle::Laser => ((205.0 + 50.0 * pos) * boost).min(255.0) as u8,
                        // BEAM: a coherent tube carries near-constant power along
                        // its length (tail still at ~70%) but tops out UNDER the
                        // laser's maximum — steady light, not a weapon discharge.
                        GlowStyle::Beam => ((165.0 + 70.0 * pos) * boost).min(240.0) as u8,
                        // Phaser: the STREAK is the point — near-constant power
                        // along its whole length (tail still at ~80%), so the
                        // fat band reads as one solid bar of spectrum trailing
                        // the cursor, not a bright head with a wispy tail. The
                        // band is a vivid TINT the glyphs render through, not a
                        // wall of light that buries them.
                        GlowStyle::Phaser => ((92.0 + 34.0 * pos) * boost).min(150.0) as u8,
                        // FIRE reads THROUGH: the baseline vein along the typed
                        // cells stays a warm tint, never a white rope burying
                        // the letters — the words render on top of the fire,
                        // and the drama lives
                        // in the curtain ABOVE the line (which re-scales in
                        // `emit_flames`) and the ember populations, not in
                        // glyph-band wash.
                        GlowStyle::Fire => ((20.0 + 78.0 * pos) * boost).min(122.0) as u8,
                        // Trail Pack birth coverage rides the pack's cov ramp
                        // (`cov_base + cov_slope·pos`, on a 0..1 grid × 255),
                        // hard-clamped to the structural legibility ceiling so a
                        // pack cannot birth an over-bright spark. Reached ONLY when
                        // `style == Custom`, so every built-in arm above is
                        // byte-identical; the `cfg.pack` read never runs for a
                        // built-in (they dispatch to their own arm).
                        GlowStyle::Custom => {
                            let (base, slope) = cfg
                                .pack
                                .as_ref()
                                .map_or((0.16, 0.69), |p| (p.beam.cov_base, p.beam.cov_slope));
                            (((base + slope * pos) * 255.0 * boost).min(Self::CUSTOM_COV_CAP)) as u8
                        }
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
        // LIGHTNING (Laser): a real jump IS a strike — the jump vector becomes a
        // jagged main channel with branch forks — and sustained hot typing
        // CRACKLES short stray arcs off the head. Geometry is forged ONCE here
        // (deterministic rng) and only its brightness animates per frame.
        // NAVIGATION-BLIND like the ring/particle gates below (and the flare slam
        // above): scrubbing to line start/end (Ctrl-A/E, Home/End) must stay calm —
        // it earns no heat, so it must forge no full lightning strike and no
        // crackle either (the flare is nav-gated upstream; the strike is the
        // other laser jump payoff that would otherwise fire on a nav leap).
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
                // Crackle: a short stray arc in a random direction, more often
                // (and only) while the run is hot — rooted anywhere along the
                // CHARGED TRAIL (a random live typing spark among the last
                // [`Self::CRACKLE_TRAIL_CELLS`], head included), so the
                // lingering residual charge visibly sparks behind the cursor
                // instead of every arc clustering at the write head.
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
        // Landing ring on a real jump (>1 cell), if enabled. Fire meteors ping
        // at ARRIVAL instead (see the strike block in `tick`). A Trail Pack OWNS
        // its ring: `p.ring.enabled` gates it (independent of the global `cfg.ring`)
        // and `p.ring.life_ms` sets its life. Every built-in (no pack) keeps the
        // `cfg.ring` gate and the 0.18 s life byte-for-byte.
        let (ring_on, ring_life) = cfg.pack.as_ref().map_or((cfg.ring, 0.18), |p| {
            (p.ring.enabled, p.ring.life_ms as f32 / 1000.0)
        });
        if ring_on && dist >= 2.0 && !fire_meteor && !navigation {
            let (cx, cy) = geom.cell_center(cr, cc);
            let scale = crate::rainbow_kitty::timing::impact(dist);
            self.ring = Some(Ring {
                cx,
                cy,
                born: now,
                life: classic_ring_life(ring_life, scale),
                scale,
            });
        }
    }

    /// The per-style particle burst for this move — jump-scaled showers,
    /// heat-scaled typing sheds; fire meteor moves defer their debris to the
    /// strike.
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
        // distance; the typing burst scales with heat (a lone keystroke sheds one
        // ember / droplet, a fast run a shower). Water gets the widest dynamic
        // range — a trickle at rest, a torrent at full heat — and a juicier jump
        // splash.
        // Fire METEOR moves defer their debris to the STRIKE (see
        // `meteor_strike_fountain`): the fountain erupts along the FINAL,
        // coalesced flight path when the head lands — never along a parked
        // intermediate hop of shell repaint choreography.
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
                    // A jump ERUPTS: a fountain of embers scaled by the leap —
                    // born all ALONG the swept path (see the fire arm below), so
                    // the whole line catches, not just the landing cell.
                    ((12.0 + dist * 1.8) as usize).min(44)
                } else if laser {
                    // The beam LANDS like a cutting torch: a hard ablation shower.
                    ((8.0 + dist * 1.3) as usize).min(30)
                } else if beam {
                    // A warp leap stirs a modest wake of STARDUST along the rod —
                    // stars, not exhaust; the rod itself is the show.
                    ((4.0 + dist * 0.5) as usize).min(14)
                } else if comet {
                    // A leap sheds a METEOR TRAIN — debris strewn along the whole
                    // swept vector (see the comet arm below), restrained enough
                    // that the tail's beam stays the star.
                    ((5.0 + dist * 0.9) as usize).min(18)
                } else if matches!(cfg.style, GlowStyle::Sparkle) {
                    // A leap SPILLS THE SKY: the widened ribbon sheds a generous
                    // train of stars, moons and mini-comets along the vector —
                    // abundance is sparkle's whole personality now.
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
                // A quiet nucleus sheds a single grain; a fast run OUTGASSES —
                // the wake fills with hanging glitter, still count-capped well
                // under fire's ember column.
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
                        // Three ember populations sell the FIRE read (the `hue`
                        // seed doubles as the ember's temperature + size, see
                        // `emit_particles`):
                        //   POP     — a white-hot spark shot fast and high, arcing
                        //             over and dying young; the crackle of a real
                        //             blaze, present only when truly hot;
                        //   SMOLDER — a slow dim mote that drifts and LINGERS long
                        //             after the keystroke (the suspense between
                        //             bursts — the fire is never quite out);
                        //   EMBER   — the classic buoyant riser, kicked harder as
                        //             the blaze climbs.
                        // On a JUMP every ember is born somewhere ALONG the leap
                        // (`r0` walks the vector), so the whole swept path catches
                        // fire behind the flare — not just the landing cell.
                        let (bx, by) = if dist >= 2.0 {
                            let (fx, fy) = geom.cell_center(pr, pc);
                            (fx + (ox - fx) * r0, fy + (oy - fy) * r0)
                        } else {
                            (ox + (r0 - 0.5) * geom.cw as f32, oy)
                        };
                        // Jump embers are METEOR DEBRIS: they die in half the
                        // time, so the wake reads as motion aftermath and the
                        // landing row is quiet again well under a second (the
                        // regression law pins it).
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
                        // Three droplet populations sell the WATER read:
                        //   DRIP   — sags off the wake, then accelerates straight
                        //            down (dominant while typing SLOW — a little
                        //            water, dripping);
                        //   SPRAY  — kicked up and out, arcing back under gravity;
                        //            its energy scales with heat (gentle beads at
                        //            rest, a churning fountain at full torpedo);
                        //   BUBBLE — buoyant cavitation rising out of the wake
                        //            BEHIND the head — the torpedo tell, present
                        //            only at sustained speed.
                        // A JUMP is a splash instead: crown droplets arcing over
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
                        // ABLATION sparks: hard, fast, short-lived — ejected with a
                        // strong SIDEWAYS bias from the impact point (grinder
                        // sparks, not confetti), then yanked down by real gravity.
                        // Rendered as velocity-aligned streaks in the beam's own
                        // hue (see the laser branch in `emit_particles`).
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
                            // Exact-point births are the cutting-torch read
                            // (jitter would blur the impact), so the shower
                            // divides its punch instead: √-scaling keeps a
                            // lone spark at full brightness (1/√1 == 1.0,
                            // byte-identical) while a 30-spark jump landing
                            // stacks to the beam hue, not clamped white.
                            cov_scale: (burst as f32).sqrt().recip(),
                            born: now,
                        }
                    }
                    GlowStyle::Beam => {
                        // STARDUST: space is WEIGHTLESS — no gravity, no arcs.
                        // Each mote is born in the rod's wake (a jump seeds them
                        // all ALONG the leap vector, `r0` walks it; typing seeds
                        // them a little behind the head) and drifts slowly,
                        // lingering like stars left hanging in the dark. Rendered
                        // as twinkling 4-point stars (see `emit_particles`).
                        let (bx, by) = if dist >= 2.0 {
                            // Window-absolute like `ox`/`oy` (the Fire arm's
                            // form). Grid-relative endpoints put the leap vector
                            // `origin` px up-left of the real departure cell, so
                            // motes decorate cells the rod never crossed (or fall
                            // above the effects box and get clipped). Adds 0.0 at
                            // origin (0,0), so the identity law holds bit-exactly.
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
                        // DEBRIS glitter: grains of shed ice. Dust HANGS — near-zero
                        // gravity, a slow drift AGAINST the motion (the tail
                        // direction), twinkling as it disperses (see the comet
                        // branch in `emit_particles`). On a jump the grains are
                        // strewn along the whole swept vector — a meteor train —
                        // instead of bursting from the landing cell.
                        // Window-absolute like `ox`/`oy` (the Fire arm's form):
                        // grid-relative fx/fy skew the drift vector below by
                        // (origin_x, origin_y) on EVERY spawn, so in a real
                        // window the shed dust drifts toward the top-left instead
                        // of trailing the motion and the stationary-re-key
                        // dispersion branch below is unreachable (`ml` never near
                        // zero). Adds 0.0 at origin (0,0) — identity-law safe.
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
                        // THE CELESTIAL POUR: stars, moons and mini-comets
                        // spill out of the rainbow ribbon. Same spread-birth
                        // law as the default arm (r2 walks the leap vector /
                        // jitters the cell, r4 scatters off the centre line),
                        // but the toss is gentler and the grains live longer —
                        // a star should HANG in the sky for a beat, not
                        // plummet like grit. `hue` doubles as the shape seed
                        // in `emit_particles` (quantized well away from the
                        // rolling colour so shape never flickers).
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
                        // Sparkles fly outward, fall under gravity, rainbow-hued.
                        // Births are SPREAD, never point-stacked: a 20-spark jump
                        // burst born at exactly (ox, oy) saturating-adds the
                        // landing cell to a flat white blob (the same pile-up
                        // Fire's `bx` walk and Water's lowered base_cov avoid).
                        // `r2`/`r4` are drawn for every arm, so consuming them
                        // here shifts NO RNG stream for any style: `r2` jitters
                        // the birth across the cell — and on a jump walks the
                        // leap vector, strewing the burst along the swept path
                        // like Fire/Comet/Beam — while `r4` scatters it off the
                        // exact centre line.
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

    /// Trail Pack particles: the pack's ≤3 ballistic populations, reached only
    /// when a pack is resolved (every built-in skips it entirely).
    pub(super) fn spawn_pack_particles(&mut self, mv: &MoveCtx, fire_meteor: bool) {
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
        // TRAIL PACK particles — a NEW opt-in branch reached ONLY when a pack is
        // resolved (`cfg.pack.is_some()`); every built-in skips it entirely, so
        // their spawn path stays byte-identical. It spawns the pack's ≤3 ballistic
        // populations into the SHARED particle collection (same `Particle` shape,
        // same `MAX_PARTICLES` cap, same life prune) — `emit_custom` draws them
        // from `p.ramp`. `cov_scale` carries the population's dot-size scale and
        // `hue` its ramp seed (this collection is drawn only by the custom path).
        if let Some(p) = cfg.pack.as_ref()
            && p.particle_count > 0
            && !fire_meteor
            && !navigation
        {
            let jump = dist >= 2.0;
            let heat = self.heat;
            let cw = geom.cw as f32;
            let cell = geom.ch as f32;
            let (ox, oy) = geom.cell_center(cr, cc);
            let (fx, fy) = geom.cell_center(pr, pc);
            for idx in 0..p.particle_count as usize {
                let pop = p.particles[idx];
                let base = if jump {
                    (pop.jump_burst_max as f32) * (0.35 + 0.65 * (dist / 12.0).min(1.0))
                } else {
                    (pop.typing_burst_max as f32) * heat
                };
                // `spawn_weight` scales this population's contribution: 0 silences it,
                // <1 thins it, >1 makes it denser. The pack's compiled per-population
                // max — itself scaled by the weight — bounds one move; MAX_PARTICLES
                // (checked in the loop) bounds the resident total.
                let cap = ((pop.jump_burst_max.max(pop.typing_burst_max) as f32) * pop.spawn_weight)
                    .ceil() as usize;
                let burst = ((base * pop.spawn_weight) as usize).min(cap);
                for _ in 0..burst {
                    if self.particles.len() >= Self::MAX_PARTICLES {
                        break;
                    }
                    let r0 = self.frand();
                    let r1 = self.frand();
                    let r2 = self.frand();
                    let r3 = self.frand();
                    let r4 = self.frand();
                    let (bx, by) = if jump {
                        (
                            fx + (ox - fx) * r0,
                            fy + (oy - fy) * r0 + (r4 - 0.5) * 0.5 * cell,
                        )
                    } else {
                        (ox + (r0 - 0.5) * cw, oy + (r4 - 0.5) * 0.4 * cell)
                    };
                    let vx = (pop.vx.0 + (pop.vx.1 - pop.vx.0) * r1) * cell;
                    let vy = (pop.vy.0 + (pop.vy.1 - pop.vy.0) * r2) * cell;
                    let life = (pop.life.0 + (pop.life.1 - pop.life.0) * r3).max(0.02);
                    self.particles.push(Particle {
                        x0: bx,
                        y0: by,
                        vx,
                        vy,
                        gy: pop.gravity * cell,
                        life,
                        hue: r4, // per-particle ramp seed (resolved in `emit_custom`)
                        cov_scale: pop.size, // dot-size scale for the custom draw
                        born: now,
                    });
                }
            }
        }
    }

    /// Forge one lightning bolt from `from` to `to`: midpoint-displacement
    /// jagging (amplitude halving each round — the classic fractal silhouette)
    /// gives the channel its kinks, FROZEN at spawn so the strike holds its
    /// shape for its whole life. A `big` (jump) strike also throws 2–4 thinner
    /// BRANCH forks off interior kinks, each jagged once itself — the forking
    /// is the tell that makes it lightning, not a wavy line. Deterministic via
    /// the resident xorshift rng.
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
            // The ONE buffer a `Bolt` owns for its life. The jag's two
            // scratches go back to the pool on the next line, so a crackle
            // costs one allocation where it used to cost three, and a jump one
            // where it used to cost five.
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

    /// The METEOR'S DEBRIS: at the strike, embers erupt ALONG the coalesced
    /// flight path — births biased toward the landing — and die fast, so the
    /// wake reads as motion aftermath and the landing row is quiet again well
    /// under a second (the newline regression law pins it).
    pub(super) fn meteor_strike_fountain(&mut self, m: &Meteor, now: Instant, geom: Geom) {
        let cell = geom.ch as f32;
        let (dx, dy) = (m.x1 - m.x0, m.y1 - m.y0);
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        let dist_cells = len / geom.cw.max(1) as f32;
        // A LINE'S debris: few sparks, shed ALONG the flight
        // direction — they continue the streak's motion with a small spread,
        // so the wake reads as a needle shedding sparks, never a billow.
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
        // EXPIRE a stale hint outright: it keeps the animation timer armed
        // (see `has_live_motion`), so an unconsumed hint must self-disarm or
        // an idle screen would tick forever on a kill that never echoed.
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
        // A poof is licensed by EITHER a kill chord (`kill_hint`) OR a plain
        // Backspace (`bs_poof_hint`) — v0.43.0 law, restored: EVERY Backspace
        // licenses the erase POOF. Both mean "text is vanishing, puff the
        // vacated span"; they share the whole downstream span/fallback
        // machinery and the `POOF_MIN_GAP` rate gate, only their arming
        // semantics differ.
        //
        // THE CUE, though, belongs to the kill CHORD alone — the licences are
        // shared, the SWOOSH is not. `spawn_poof`'s own contract has always
        // said so ("Plain Backspace already spoke on its admitted cursor
        // retreat; its exact poof is visual-only so one physical key never
        // clicks twice"), and its `cue_kill` parameter exists to express it —
        // but both call sites passed a literal `true`, so every plain
        // Backspace whose poof fired ALSO emitted a TIER-3 `Kill` swoosh on
        // top of its own TIER-1 voice. That is 7 dB of falling noise over the
        // erase poof the owner asked to hear, which is a straightforward way
        // to bury it. `kill_chord` is the parameter's intended value.
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
        // WHICH KEY LICENSED THIS POOF — hoisted above BOTH branches because it
        // now picks the poof's VOICE as well as gating the caret fallback's
        // erasure witness (see [`PoofVoice`] and the fallback's own note). It
        // must be read before the span branch, which CONSUMES `kill_hint` when
        // it fires.
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
            // A ContentOnly probe (plain alt screen — no repaint blink) carries
            // no kill license: Ctrl-U there is a page scroll, and the region
            // scroll shrinking the probed row is not an erase (see
            // [`ProbeTrust`]). Both halves of the diff must be full-trust.
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
            // Common SUFFIX aligned at the two FILLS (survivors of a Ctrl-U /
            // word-kill shift LEFT, so their tails line up at the fills, not at
            // the buffer ends), capped at the survivor's fill.
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
            // STABLE SURVIVORS: everything still on the row is accounted for by
            // the unmoved prefix + the shifted suffix (overlap allowed — repeated
            // chars make p and s double-count, which only proves stability
            // harder). A scrolled/replaced row shares no such structure.
            //
            // …but the test is VACUOUSLY true when the new row is BLANK
            // (`cur_fill == 0` accounts for nothing), and a page-scroll can
            // slide in a replacement row that shares an accidental prefix
            // ('    })' → '    }'). Both are LINE REPLACEMENTS, not kills, so
            // the arm additionally demands a NON-TRIVIAL SURVIVOR below.
            if cur_fill > 0 && p + s >= cur_fill {
                // The vanished span: prefix end .. old fill minus the survivors
                // that shifted in from the right. `s_span` re-caps the suffix so
                // repeated-char overlap can't push `c1` left of `c0`, and the
                // PREV caret refines `c0` for forward kills inside repeated text
                // (Delete on "aaaa" — the diff alone can't see WHICH 'a' died,
                // the caret can; kills never erase left of a survivor prefix
                // that the caret sits inside).
                let s_span = s.min(cur_fill - p.min(cur_fill));
                let c0 = p.min(prev.caret as usize).min(cur_fill) as u16;
                let c1 = (prev_fill - s_span).max(c0 as usize + 1) as u16;
                // NON-TRIVIAL SURVIVOR: at least one NON-BLANK cell of the
                // baseline's content must survive OUTSIDE the vanished span —
                // the unmoved prefix left of `c0` holds ink, or a shifted
                // suffix exists (its last cell is `cur_fill - 1`, non-blank by
                // the fill definition). A real kill always leaves such a
                // remnant abutting the span (prompt, prefix, or the shifted
                // tail); a page-scroll REPLACEMENT whose caret sits inside the
                // blank margin pulls `c0` left of every surviving glyph
                // ('    })' → '    }' with the caret at the line start) and a
                // blank scroll-in retains nothing at all — both refuse here
                // and stay dark instead of minting a phantom full-row poof.
                // A refusal simply falls through — the licences keep their
                // freshness windows (a later batch may still carry the real
                // shrink) and the Full-trust caret fallback below may answer.
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
        // CARET-ANCHORED FALLBACK: every kill keypress earns exactly one poof.
        // The span branch above answers precisely when the row shrank in
        // place (a plain shell's Ctrl-K/EL); everything else observed live in
        // Claude Code defeats any row diff — its bottom-anchored Ink box
        // REFLOWS on a kill, so the survivor line is replaced AND re-rowed at
        // once. The HINT is already the proof a kill key was pressed (repaint
        // storms never arm it), so answer at the caret with a modest fixed
        // puff; the one over-fire left — a kill with nothing to kill — still
        // reads as honest feedback. Requires a probe this frame (headless /
        // scrolled-back frames stay quiet) and respects the rate gate (a held
        // kill key poofs at most once per POOF_MIN_GAP).
        // The kill-chord grace gives the precise span branch first crack for
        // `POOF_FALLBACK_GRACE`, then the caret fallback answers.
        let poof_hint_at = match (self.kill_hint, self.bs_poof_hint) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        // A plain Backspace must PROVE it erased something before the caret
        // fallback answers. The kill-CHORD fallback stays permissive — its hint
        // is proof a kill KEY was pressed, and "a kill with nothing to kill
        // still reads as honest feedback" (the reflowing-TUI case the fallback
        // exists for). But an ordinary Backspace at column 0 / on an empty row
        // erases NO cell, so when the ONLY licensing hint is `bs_poof_hint`
        // (kill hint absent) we demand real-shrink evidence.
        //
        // TWO WITNESSES, because one of them is blind exactly when it matters.
        // `erasure_proven` is the live frame-to-frame diff; it only exists while
        // frames are flowing, and a lone correction after a pause is by
        // definition the case where they are not (see [`Self::bs_baseline`]).
        // `bs_erased` asks the same question against the row stamped AT THE KEY,
        // which is the only observation that predates the erase in that case.
        // (`bs_only` is hoisted above the span branch, which consumes the hint.)
        let erasure_proven = self
            .row_prev_meta
            .zip(self.row_cur_meta)
            .is_some_and(|(prev, cur)| prev.row == cur.row && cur.fill < prev.fill);
        // …and the same question asked against the row as it stood AT THE KEY
        // ([`Self::bs_baseline`]), which survives the frame gap the live diff
        // cannot. With no baseline stamped the host simply was not composing,
        // so "no shrink observed" proves nothing; fall back to the honest
        // weaker witness — the caret is off column 0, so a Backspace here COULD
        // have erased. A no-op Backspace at the left margin (and its autorepeat
        // there) still stays silent, which is the case this gate exists for.
        let erase_on_glass = self.poof_erase_witnessed();
        let bs_erased = erase_on_glass
            || matches!(
                (self.bs_baseline, self.row_cur_meta),
                (None, Some(cur)) if cur.caret > 0
            );
        // THE GRACE, OR THE PROOF IT WAS WAITING FOR. The grace buys the precise
        // span branch first shot at the echo; `erase_on_glass` says the echo is
        // ALREADY on the probe this frame and the span branch (which ran above,
        // on this same probe) declined it. Waiting out the rest of the timer
        // then buys nothing but latency — and it is not a small latency: from a
        // QUIET screen the erase's own damage drives this tick within a frame of
        // the keypress, so the poof used to sit out ~50 ms of grace watching a
        // gap the user is already looking at.
        //
        // WHY THIS CANNOT STEAL THE SPAN BRANCH'S SHOT. The span branch answers
        // by diffing the PREVIOUS probe against this one. For it to match on a
        // LATER frame it needs a shrink between this probe and that one — i.e.
        // the erase would still have to be in the future — which is exactly what
        // `erase_on_glass` rules out: the caret's row is already SHORTER than it
        // was at the key. A second shrink after that is a second erase, and
        // `POOF_MIN_GAP` (not the grace) is what governs those.
        //
        // The timed grace stays as the cap for every case with no witness: a
        // reflow that re-rowed the caret, a kill whose echo never shows, a host
        // that had no probe to stamp at the key.
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
        // Rotate only when a FRESH probe arrived this frame; an unprobed
        // frame (scrolled back, split pane unwired) keeps the previous truth
        // in place instead of forgetting it. The content witnesses' neighbor
        // captures rotate in the SAME motion: their validity states ride the
        // probe metadata, so meta and buffers stay one unit (a fresh probe
        // without a neighbor capture rotates `Unprobed` states in, and the
        // stale neighbor bytes become unreadable by construction).
        if self.row_cur_meta.is_some() {
            std::mem::swap(&mut self.row_prev, &mut self.row_cur);
            std::mem::swap(&mut self.row_above_prev, &mut self.row_above_cur);
            std::mem::swap(&mut self.row_below_prev, &mut self.row_below_cur);
            self.row_prev_meta = self.row_cur_meta.take();
        }
    }

    /// Emit one erase POOF over the vanished span `[c0..c1)` of `row` (`n` =
    /// net columns erased) in the STYLE'S OWN language — always additive light
    /// or source-over veils, never squares over glyphs, so surviving text stays
    /// legible:
    /// - default (Lumen/Laser/Beam/Phaser): the owner's "little smoke poof" —
    ///   neutral-grey [`VaporKind::Poof`] puffs with the hot-cursor smoke's
    ///   buoyant motion but a short life (a puff, not a chimney);
    /// - FIRE: QUENCH STEAM, verbatim reuse of the deletion-echo burst params —
    ///   water on hot steel is fire's established deletion language, and
    ///   [`Self::note_kill`]'s quench escalation makes the standing flame die
    ///   down at the same moment;
    /// - RAINBOW KITTY: the owner's "little sparkles" — star-power particles strewn
    ///   across the span (SPARKLE shares them, its native language);
    /// - WATER: droplets spraying up and falling out of the span — the text
    ///   splashes away;
    /// - COMET: ice glints — shattered frost.
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
        // SOUND — THE CLOUD'S OWN LITTLE NOISE. The poof is the one visual an
        // erase always earns, so its cue rides this edge and inherits the same
        // `POOF_MIN_GAP` rate limit the light does.
        //
        // WHICH noise depends on WHICH KEY licensed it, because the two are not
        // the same gesture and were never meant to sound alike:
        //
        // * a LINE KILL (^U/^K) is a whole clause leaving at once, and it
        //   arrives with no keystroke of its own — the downward
        //   [`SoundKind::Kill`] swoosh is the erase's entire voice.
        // * a WORD KILL (^W, Alt-D, Alt/Ctrl-Backspace) is one word going:
        //   [`SoundKind::KillWord`], the erase poof one size up — not the
        //   clause's swoosh.
        // * a plain BACKSPACE has ALREADY SPOKEN, on its admitted cursor
        //   retreat — the two-band erase POOF — and stacking the full kill
        //   swoosh on top of it made one physical key say two big things at
        //   once — the exact double-click this function's own comment used to
        //   forbid and the code did anyway (both branches passed
        //   `cue_kill = true`). It now gets [`SoundKind::Poof`]: the same
        //   noise-poof synthesis at cloud scale, the AIR of the poof the eye
        //   sees, quiet enough to ride under the erase voice it accompanies
        //   instead of fighting it.
        match voice {
            PoofVoice::Swoosh => self.cue_sound(crate::trail_sound::SoundKind::Kill, c0),
            PoofVoice::WordPoof => self.cue_sound(crate::trail_sound::SoundKind::KillWord, c0),
            PoofVoice::Puff => self.cue_sound(crate::trail_sound::SoundKind::Poof, c0),
        }
        // SEAM POINT 2 (§17.2, D14): a KILL reaches v2 from HERE, the erase
        // detector, rather than from `note_kill` — this is the one place that
        // holds both the PROVEN span (`n`: §5.6's "1 star per 3 cells" and
        // §8.2's `12·n + 240` drain) and the licence's scale (`voice`), and
        // by now the caret is where the kill left it, which is the cell the
        // engine retracts from. A plain Backspace's puff mints nothing: its
        // `Erase` went out at the key. A caret-MOVING kill's `Kill` went out
        // with its retreat (`kill_reported_to_v2`, the move seam) and is not
        // minted twice; a stationary kill's, or a moving kill's whose poof
        // answered before its retreat landed, is minted here — once per
        // kill, keyed on the kill hint's own stamp. The hint is still armed
        // here: the branches that call this clear it after.
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
        // LIGHT THEMES: additive particles saturate against a white ground —
        // the particle-only styles (rainbow kitty/Sparkle/Water/Comet) ALSO shed a few
        // neutral grey puffs there (rendered through `push_halo_over`'s
        // source-over veil path, which light themes already use), so the poof
        // reads on paper-white exactly as it does on black.
        //
        // DARK THEMES TOO (owner, 2026-08-29: *"I'm still not seeing the cloud
        // poof for backspace"*). The dark skip was photometric, not a ruling —
        // "the particles carry it alone" — and it meant the shipped default
        // theme never showed the puff at all. The renderer already carries a
        // dark-ground arm for `VaporKind::Poof` (neutral grey darkening as it
        // thins, alpha 58 against the light arm's 140), so the only thing
        // gated on the theme here was the spawn. A delete now reads as
        // REMOVAL on every ground: grey, buoyant, dispersing — not as the
        // saturated rising glitter that means creation.
        if matches!(
            cfg.style,
            GlowStyle::RainbowKitty | GlowStyle::Sparkle | GlowStyle::Water | GlowStyle::Comet
        ) {
            self.spawn_poof_smoke(span_x0, span_w, oy, cell, cwf, n, now);
        }
        match cfg.style {
            GlowStyle::RainbowKitty | GlowStyle::Sparkle => {
                // §5.6: under v2 the erase's stars are stardust's (3 m3 + 1 m2
                // thrown off the cell); only the SMOKE above is v1-owned and
                // kept. The glitter cloud is v1's erase grammar, so it lays
                // nothing while v2 owns the frame.
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
                // WATER: droplets spray up out of the span and fall under real
                // gravity (the SPRAY population's params); COMET: the same
                // ballistics read as ice glints through its near-white→hue
                // twinkle ramp, settling instead of plummeting.
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
                // QUENCH STEAM, the deletion-echo burst verbatim (billowing
                // puffs + every-third hiss speck, drag-stalled rise), scaled by
                // the span and spread across it — a killed line flashes off the
                // hot page while note_kill's quench visibly damps the blaze.
                let puffs = ((1.0 + nf.sqrt()) as usize).clamp(1, 5);
                for k in 0..puffs {
                    if self.vapor.len() >= Self::MAX_VAPOR {
                        break;
                    }
                    let (r0, r1, r2) = (self.frand(), self.frand(), self.frand());
                    let hiss = k % 3 == 2;
                    self.vapor.push(Vapor {
                        x0: span_x0 + ((k as f32 + 0.5) / puffs as f32) * span_w + (r0 - 0.5) * cwf,
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
            _ => {
                // The default LITTLE SMOKE POOF (Lumen/Laser/Beam/Phaser).
                self.spawn_poof_smoke(span_x0, span_w, oy, cell, cwf, n, now);
            }
        }
    }

    /// THE ERASE SPARKLE POOF — ONE implementation, both entry points.
    ///
    /// The rainbow kitty / Sparkle language for "text went away": a scatter of
    /// GLITTER twinkles off the vanished span plus one bright white-cored pop at
    /// its centre. Called by the span detector ([`Self::spawn_poof`], which
    /// knows the exact erased range) AND by the deletion ECHO in
    /// [`Self::spawn`] (which sees one vacated cell as the caret steps left).
    /// They used to be two hand-written particle loops that had drifted apart —
    /// different counts, different ballistics, only one of them with a pop — so
    /// a single Backspace looked like a different gesture depending on which
    /// detector happened to answer first.
    ///
    /// ENERGY is [`erase_poof_drive`]: momentum × weight. It sets the star
    /// COUNT, the launch SPEED, and the LIFE, so "deleting fast" and "deleting a
    /// lot" both read immediately and independently.
    ///
    /// BALLISTICS mirror the typing wake. Erasing walks the caret LEFT, so the
    /// debris streams RIGHT — behind the direction of travel, exactly as the
    /// typing plume streams left behind a caret walking right. That is the
    /// "similar momentum as forward typing" the owner asked for: the same
    /// physical read, pointed the other way.
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
        // UP, UNLESS THERE IS NO UP. The debris rises; on the FIRST grid row
        // there is nowhere to rise to, so a third of every poof was thrown
        // past the top of the viewport and clipped mid-sparkle — a half-effect
        // for anyone deleting on line one, which is where a shell prompt lives.
        // Flip the bias downward when the row cannot afford the lift.
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
        // The debris CLUSTERS at the collapse point (see
        // [`ERASE_POOF_SPREAD_CELLS`]): `f²` biases it toward the span's start
        // and the width is capped, so a line kill is a burst where the line went
        // rather than a dotted rule where the line was.
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
                // Gravity is DOWN regardless of which way the debris was
                // thrown: multiplying it by `lift` made a downward poof
                // accelerate back UP and off the top of the viewport, which is
                // the clipping a white-ground review kept seeing on row 0.
                gy: 0.9 * cell,
                life: (0.42 + r4 * 0.55) * (0.85 + 0.45 * heft),
                hue: r0, // twinkle seed
                // ROUND-SPARK sentinel ([`ERASE_ROUND_SCALE`], still inside
                // the GLITTER band `< 0.9`): the delete poof is a DELETION
                // gesture, not the momentum shower, so it keeps FULL
                // brightness regardless of TYPING momentum (the
                // `emit_particles` glitter arm pins `mom = 1.0`). Plain
                // twinkle stars would take `rainbow_star_momentum(rainbow.disp)`
                // instead and go barely-there on a cold delete. The BODY
                // draws ROUND — the old-style spark — never the 4-point plus;
                // the hero below is the poof's ONE plus.
                cov_scale: ERASE_ROUND_SCALE,
                born: now,
            });
        }
        // ONE bright white-cored central POP flash at the erased span — an
        // unmistakable spark at the exact vanished glyphs so even a lone
        // Backspace reads immediately. Near-stationary (pops in place, tiny
        // drift), short life, `hue < 0.4` so the glitter arm tints it pure
        // white; TYPING-momentum-independent like the sparkles above, but its
        // life carries the span's heft so a line kill's pop lingers.
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
                // `< 0.4` still selects the WHITE core on dark; on light the
                // same seed selects a BAND, and 0.02 lands on the family's RED.
                // It used to resolve `hsv2rgb(hue, 0.9, 1.0)`, where 0.2 was a
                // yellow-green that `light_ink` turned into the "muddy olive
                // smudge" a white-ground review put at the centre of the poof —
                // the snap to named stops makes that whole failure mode
                // unreachable, for every seed and not just this one. Red reads
                // as a spark on both grounds, so the choice stands on its own.
                hue: 0.02,
                // THE HERO. Still inside the GLITTER band (`< 0.9`), but in the
                // reserved top of it: `emit_particles` reads 0.80..=0.89 as
                // "draw this one BIG", scaled by the heft encoded in the
                // fraction. A poof whose grains are all one size cannot say how
                // much vanished — a white-ground review put it exactly that
                // way, "a 1-char delete and a 5-char delete would look
                // identical". This is the one grain that does.
                cov_scale: ERASE_HERO_SCALE_MIN + ERASE_HERO_SCALE_SPAN * heft,
                born: now,
            });
        }
    }

    /// The neutral-grey SMOKE POOF puffs — THE CLOUD: the hot-cursor smoke's
    /// buoyant motion with a SHORT life (a puff, not a chimney). It is the poof's
    /// BODY in every style that does not draw one of its own — the whole poof for
    /// the default styles, and the billow the rainbow-kitty / Sparkle glitter
    /// rides on (on light grounds it also carries them, since additive sparkles
    /// saturate against white).
    ///
    /// The puffs CLUSTER at the collapse point on exactly the law the sparkles
    /// obey ([`ERASE_POOF_SPREAD_CELLS`] and its `f²` bias) — see the tuning
    /// note inside.
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
        // WEIGHT, on the same law the sparkles use: a killed line billows where
        // a single character wisps. The old `clamp(1, 5)` saturated at nine
        // erased columns, so on a white ground — where these grey puffs ARE the
        // poof's body — a word kill and a line kill were the same puff.
        let puffs = ((1.0 + f32::from(n).sqrt() * 1.4) as usize).clamp(1, 8);
        // …AND THEY CLUSTER, on the sparkles' own [`ERASE_POOF_SPREAD_CELLS`]
        // law. Spreading `puffs` evenly over the WHOLE span inverted the weight
        // it had just computed: a 44-column Ctrl-U dealt its 8 puffs across 44
        // cells — one thin wisp every five columns, measurably FAINTER on glass
        // than the 2 overlapping puffs a one-character Backspace got. A cloud is
        // a cloud because its puffs overlap, so the spread is capped and `f²`
        // biases the births toward the span's start: a line kill is a BILLOW
        // where the line collapsed, not a dotted rule where the line was — the
        // same sentence [`Self::spawn_erase_sparkles`] writes with grains.
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
                // IT HAS TO GO SOMEWHERE. At the old velocities a puff rose
                // 0.28-0.5 cell PER SECOND against a ~0.8 s life — under a tenth
                // of a cell across the whole animation, i.e. a grey blob that
                // appeared, sat exactly where it was born, and faded. Read on
                // glass as "the caret went fuzzy", not as smoke. Roughly doubled
                // (and given a little more lateral spread), the cloud now visibly
                // LIFTS and DISPERSES as it thins — the dissipation IS the poof.
                // Still buoyant, still short: a puff, not a chimney.
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
}
