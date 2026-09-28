// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TYPING HEAT — the momentum-driven heat every style reads and the fire
//! style's forge temperature.

use super::*;

impl CursorGlow {
    /// The wake brightness multiplier for TYPING light at the current heat: cool
    /// typing is a whisper (~0.28×), sustained fast typing overdrives past the
    /// configured intensity (~1.2×) — the "acceleration" feel. Jump comets and
    /// landing rings are navigation feedback, not typing, so they stay at 1×.
    pub(super) fn typing_boost(&self) -> f32 {
        0.28 + 0.92 * self.heat
    }

    /// The RAW blaze level 0..1: the typing heat or the jump flare, whichever
    /// burns hotter — instant, un-eased. Non-fire styles (the laser impact
    /// flash) read this; fire reads the eased [`Self::fire_t`] instead. Public
    /// so the host can hand the SAME surge to the signature cursors (e.g.
    /// [`crate::cursor_droplet::CursorDroplet`] — read it after
    /// [`Self::tick`], which applies the lazy heat/flare decay).
    #[must_use]
    pub fn blaze(&self) -> f32 {
        self.heat.max(self.flare).clamp(0.0, 1.0)
    }

    /// The cadence CREDIT one keystroke at inter-key gap `gap` earns: full at
    /// ≤ [`Self::HEAT_GAP_FULL`], none at ≥ [`Self::HEAT_GAP_ZERO`]. Hoisted
    /// out of the echo's heat ramp so the key-time click's timbre prediction
    /// reads the SAME curve — two copies of this would drift and the click
    /// would slowly stop matching the light it is the twin of.
    pub(super) fn heat_cadence(gap: f32) -> f32 {
        1.0 - ((gap - Self::HEAT_GAP_FULL) / (Self::HEAT_GAP_ZERO - Self::HEAT_GAP_FULL))
            .clamp(0.0, 1.0)
    }

    /// The per-keystroke heat GAIN this style/pack charges. Fire earns its
    /// blaze over ~30-40 keys; a Trail Pack may override (custom only);
    /// `None` and every built-in keep the shared [`Self::HEAT_GAIN`]
    /// byte-for-byte.
    pub(super) fn heat_gain(cfg: &GlowConfig, fire: bool) -> f32 {
        if fire {
            Self::FIRE_HEAT_GAIN
        } else if let Some(g) = cfg.pack.as_ref().and_then(|p| p.heat.gain) {
            g
        } else {
            Self::HEAT_GAIN
        }
    }

    /// The heat COOLING τ this style/pack runs. Fire cools on its own slower τ
    /// (momentum survives a short thought); a Trail Pack may override (custom
    /// only); `None` and every built-in keep [`Self::HEAT_DECAY_TAU`]
    /// byte-for-byte.
    pub(super) fn heat_tau(cfg: &GlowConfig) -> f32 {
        if matches!(cfg.style, GlowStyle::Fire) {
            Self::FIRE_HEAT_TAU
        } else if let Some(tau) = cfg.pack.as_ref().and_then(|p| p.heat.tau) {
            tau
        } else {
            Self::HEAT_DECAY_TAU
        }
    }

    /// Fire's eased DISPLAY TEMPERATURE 0..1 — the one number every fire layer
    /// reads: coal-floored (relaxed typing keeps a small live flame), quench-
    /// damped (deleting douses it), attack/release-eased (the whole look ramps
    /// as one body; nothing pops). Evolved once per tick in the lazy-decay
    /// block, BEFORE any spawn, so every consumer in one frame agrees.
    pub(super) fn fire_t(&self) -> f32 {
        self.disp_t.clamp(0.0, 1.0)
    }

    /// The FORGE cursor fill for the fire style — ALWAYS warm metal: a dull
    /// EMBER at rest (never the jarring theme colour the block cursor otherwise
    /// shows), climbing the
    /// black-body ramp — cherry → orange → hot yellow — as the metal forges hot
    /// with sustained momentum. The renderer's fill-override contrast floor
    /// (the seam the rainbow cursor rides) keeps the glyph beneath readable.
    /// Quantized to u8 channels by construction, so consecutive frames with an
    /// imperceptible temperature change fingerprint identically and early-out —
    /// a cool cursor is a STATIC ember (stable fp, timer disarms; the heating
    /// arc keeps the timer armed via `is_active`'s `cursor_temp` term).
    pub fn forge_fill(&self) -> Option<u32> {
        let t = ((self.cursor_temp - Self::FORGE_MIN_TEMP) / (1.0 - Self::FORGE_MIN_TEMP))
            .clamp(0.0, 1.0);
        // The metal BREATHES: a slow molten shimmer rides the field phase
        // (deterministic; quantized to u8 by the ramp, so idle settles).
        let breathe = 0.045 * t * (self.flame_phase * 1.7).sin();
        // Ramp floor 0.10 = a dull ember even stone-cold; never returns None,
        // so the fire cursor is warm the instant the style is active.
        Some(fire_ramp((0.10 + 0.64 * t + breathe).clamp(0.0, 0.86)))
    }

    /// Zero the THERMAL integrators (heat / flare / coal / quench, the eased
    /// display temperatures, the rainbow kitty momentum spine, the flame + specular
    /// phases, the typing cadence clock) — the full-dark half that master-off
    /// and [`Self::reset`] add on top of [`Self::clear_transient_state`]. The
    /// zero-amplitude path deliberately does NOT call this: a momentary
    /// unfocus must only cool by the elapsed gap (the lazy decay), so minutes
    /// of earned forge momentum survive a focus blip.
    pub(super) fn clear_thermals(&mut self) {
        self.heat = 0.0;
        self.heat_at = None;
        self.flare = 0.0;
        self.coal = 0.0;
        self.quench = 0.0;
        self.disp_t = 0.0;
        self.cursor_temp = 0.0;
        self.flame_phase = 0.0;
        self.momentum.reset();
        // The ERASE metric is a thermal integrator like the rest: it belongs in
        // the full-dark wipe and NOT in the zero-amplitude path, for exactly
        // the reason the doc above gives — a focus blip must only cool it by
        // the elapsed gap, which the lazy decay already does.
        self.erase_mom.reset();
        self.last_type = None;
    }

    /// Typing cadence → the thermal integrators (heat / coal / the canonical
    /// typing-momentum metric), plus the quench-steam flash on a deletion
    /// echo. Returns the observed inter-key `gap` (`f32::MAX` off the typing
    /// path) — the life/chain math downstream keys on it.
    pub(super) fn update_typing_thermals(&mut self, mv: &MoveCtx) -> f32 {
        let &MoveCtx {
            pr,
            pc,
            cc,
            now,
            cfg,
            geom,
            wrap,
            re_anchor,
            rainbow_coalesce,
            typed_hinted,
            bs_pair,
            typing,
            fire,
            forward,
            deletion,
            navigation,
            ..
        } = mv;
        // Typing cadence → HEAT: each keystroke earns credit by how quickly it
        // followed the previous one (full at ≤HEAT_GAP_FULL, none at ≥HEAT_GAP_ZERO),
        // so only SUSTAINED fast typing ramps the wake up — one quick correction
        // doesn't flare, and the ramp reads as acceleration. The observed `gap` is
        // hoisted out of the heat update because the life math below ALSO keys on
        // it (typing-continuity chaining).
        if typing {
            let gap = self
                .last_type
                .map(|t| now.saturating_duration_since(t).as_secs_f32())
                .unwrap_or(f32::MAX);
            // Forward-only momentum is a FIRE law (the design brief's
            // "sustained forward momentum heats it"); the other styles keep
            // their shipped direction-blind heat byte-identically.
            if (forward || !fire) && !deletion && !navigation {
                // Both terms are hoisted into shared helpers — the key-time
                // click reconstructs this exact ramp to time-align its TIMBRE
                // with the light (see `cue_keystroke`), and a second copy of
                // the curve here would silently drift away from it.
                let cadence = Self::heat_cadence(gap);
                let gain = Self::heat_gain(cfg, fire);
                self.heat = (self.heat + cadence * gain).min(1.0);
                // The COAL BED charges on a much wider cadence window: human-
                // rhythm writing (300-400ms/key) earns most of its credit, so
                // the bed slowly builds a persistent ember floor over a
                // sentence or two — the stretched momentum arc. Fire-only.
                if fire {
                    let coal_cred = 1.0
                        - ((gap - Self::COAL_GAP_FULL)
                            / (Self::COAL_GAP_ZERO - Self::COAL_GAP_FULL))
                            .clamp(0.0, 1.0);
                    self.coal = (self.coal + coal_cred * Self::COAL_GAIN).min(1.0);
                }
            }
            // THE CANONICAL TYPING-MOMENTUM METRIC builds here and ONLY here:
            // a non-delete printable typing advance — a forward glyph echo, a
            // typing wrap/re-anchor, or a coalesced multi-glyph echo (letters
            // arriving late is still letters arriving). Backward/vertical
            // scrubbing, deletion echoes, and navigation leaps add nothing;
            // deletes/kills DRAIN at their key hints instead
            // ([`Self::note_backspace`]/[`Self::note_kill`]). Rate
            // normalization lives inside [`TypingMomentum::advance`], so a
            // coalesced 3-glyph echo credits its one observed gap — key count
            // never buys momentum. Style-independent on purpose (a mid-run
            // style switch to rainbow kitty arrives with its earned warmth); only the
            // Rainbow kitty spine consumes it today.
            //
            // TYPED CORRELATION (`typed_pair`), the "earned by real typing
            // ONLY" law: a forward same-row echo ALONE is program output — a
            // TUI printing one glyph per frame advances the caret +1 col with
            // no keystroke behind it, and would drive momentum to 1.0 with
            // zero keys typed. Requiring the fresh committed-press hint the
            // wrap/coalesce arms already correlate against
            // ([`Self::note_typed`], armed by the host on a real printable key)
            // gates EVERY arm — forward, wrap, coalesce — on a real keystroke,
            // so neither PTY output nor a non-echoing keystream can buy the cat
            // or the stars. The one residual that CANNOT be resolved at the
            // terminal layer: a printable key whose echo IS a forward move
            // (vim `l`/`w` in normal mode) is byte-indistinguishable from typed
            // text — noted, never mode-detected. The pulse below mirrors this
            // exact correlated advance onto the cursor cat so the two momentum
            // instances read one value ([`Self::take_momentum_pulse`]).
            // THE 0.43 RESTORATION LAW (amplitude, not provenance): the spine
            // advances on the PRESS-HINT half — a fresh key-time `typed_at`
            // stamp with a typing shape — NOT on candidate admission, so
            // streaming collisions that deny every candidate still let real
            // typing earn its starfield. Cold output arms no key hint and
            // builds nothing; a DENIED backspace landing wearing a wrap shape
            // (`bs_pair`) is excluded, deletes DRAIN at their key hints, and
            // one press credits at most one advance (the hint is consumed).
            if typed_hinted
                && (forward || (wrap && !bs_pair) || rainbow_coalesce)
                && !deletion
                && !navigation
            {
                self.momentum.advance(now);
                let kind = if wrap && cc < pc {
                    CursorCatMotionKind::FoldForward
                } else {
                    CursorCatMotionKind::Advance
                };
                // A held park's FOLD, flushed on this very tick ahead of the
                // key that followed it (the box-growth wrap under the park
                // rule, [`HeldPark`]), keeps the tick's pulse: the cat turns
                // the corner it really turned, and the advance behind it is
                // the same momentum either way.
                let fold_held = kind == CursorCatMotionKind::Advance
                    && self
                        .momentum_pulse
                        .is_some_and(|p| p.kind == CursorCatMotionKind::FoldForward);
                if !fold_held {
                    self.momentum_pulse = Some(CursorCatMotionPulse { at: now, kind });
                }
            } else if deletion && re_anchor && cc > pc && !navigation {
                // The input seam already delivered `CursorCat::on_key(false)`
                // for this Backspace. This pulse carries only the authenticated
                // PLACE change: duplicating the key here would drain momentum
                // and re-kick the oops reaction twice.
                self.momentum_pulse = Some(CursorCatMotionPulse {
                    at: now,
                    kind: CursorCatMotionKind::FoldReverse,
                });
            }
            self.last_type = Some(now);
            // QUENCH STEAM: a deletion echo flashes vapor off the doused cell
            // — a puff or three scaling with the quench meter (a lone delete
            // hisses subtly; a run visibly steams), plus a couple of brief
            // hiss specks. Expands fast, stalls, dies young.
            if deletion && fire {
                let cell = geom.ch as f32;
                let ox = geom.origin_x as f32 + (pc as f32 + 0.5) * geom.cw as f32;
                let oy = geom.origin_y as f32 + (pr as f32 + 0.2) * cell;
                let puffs = 2 + (self.quench * 4.0) as usize;
                for k in 0..puffs.min(6) {
                    if self.vapor.len() >= Self::MAX_VAPOR {
                        break;
                    }
                    let (r0, r1, r2) = (self.frand(), self.frand(), self.frand());
                    // Most are billowing puffs; every third is a HISS speck —
                    // a tiny, bright, very short-lived fleck of flash-steam.
                    let hiss = k % 3 == 2;
                    self.vapor.push(Vapor {
                        x0: ox + (r0 - 0.5) * geom.cw as f32,
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
            gap
        } else {
            f32::MAX
        }
    }
}
