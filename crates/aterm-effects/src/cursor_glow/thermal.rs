// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing heat: the momentum-driven heat every style reads and the fire
//! style's forge temperature.

use super::*;

impl CursorGlow {
    /// The wake brightness multiplier for typing light at the current heat:
    /// cool typing is a whisper (~0.28×), sustained fast typing overdrives
    /// past the configured intensity (~1.2×). Jump comets and landing rings
    /// are navigation feedback, so they stay at 1×.
    pub(super) fn typing_boost(&self) -> f32 {
        0.28 + 0.92 * self.heat
    }

    /// The raw blaze level 0..1: the typing heat or the jump flare, whichever
    /// burns hotter, un-eased. Non-fire styles (the laser impact flash) read
    /// this; fire reads the eased [`Self::fire_t`]. The host hands the same
    /// surge to the signature cursors (e.g.
    /// [`crate::cursor_droplet::CursorDroplet`]); read it after
    /// [`Self::tick`], which applies the lazy heat/flare decay.
    #[must_use]
    pub fn blaze(&self) -> f32 {
        self.heat.max(self.flare).clamp(0.0, 1.0)
    }

    /// The cadence credit one keystroke at inter-key gap `gap` earns: full at
    /// ≤ [`Self::HEAT_GAP_FULL`], none at ≥ [`Self::HEAT_GAP_ZERO`]. Shared
    /// with the key-time click's timbre prediction so the click matches the
    /// light it is the twin of.
    pub(super) fn heat_cadence(gap: f32) -> f32 {
        1.0 - ((gap - Self::HEAT_GAP_FULL) / (Self::HEAT_GAP_ZERO - Self::HEAT_GAP_FULL))
            .clamp(0.0, 1.0)
    }

    /// The per-keystroke heat gain this style/pack charges. Fire earns its
    /// blaze over ~30-40 keys; a custom Trail Pack may override; `None` and
    /// every built-in keep the shared [`Self::HEAT_GAIN`].
    pub(super) fn heat_gain(cfg: &GlowConfig, fire: bool) -> f32 {
        if fire {
            Self::FIRE_HEAT_GAIN
        } else if let Some(g) = cfg.pack.as_ref().and_then(|p| p.heat.gain) {
            g
        } else {
            Self::HEAT_GAIN
        }
    }

    /// The heat cooling τ this style/pack runs. Fire cools on its own slower
    /// τ (momentum survives a short thought); a custom Trail Pack may
    /// override; `None` and every built-in keep [`Self::HEAT_DECAY_TAU`].
    pub(super) fn heat_tau(cfg: &GlowConfig) -> f32 {
        if matches!(cfg.style, GlowStyle::Fire) {
            Self::FIRE_HEAT_TAU
        } else if let Some(tau) = cfg.pack.as_ref().and_then(|p| p.heat.tau) {
            tau
        } else {
            Self::HEAT_DECAY_TAU
        }
    }

    /// Fire's eased display temperature 0..1, the one number every fire layer
    /// reads: coal-floored (relaxed typing keeps a small live flame),
    /// quench-damped (deleting douses it), attack/release-eased (the whole
    /// look ramps as one body). Evolved once per tick in the lazy-decay block,
    /// before any spawn, so every consumer in one frame agrees.
    pub(super) fn fire_t(&self) -> f32 {
        self.disp_t.clamp(0.0, 1.0)
    }

    /// The forge cursor fill for the fire style, always warm metal: a dull
    /// ember at rest, climbing the black-body ramp (cherry → orange → hot
    /// yellow) with sustained momentum. The renderer's fill-override contrast
    /// floor keeps the glyph beneath readable. Quantized to u8 channels, so
    /// frames with an imperceptible temperature change fingerprint identically
    /// and early-out: a cool cursor is a static ember (the heating arc keeps
    /// the timer armed via `is_active`'s `cursor_temp` term).
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

    /// Zero the thermal integrators (heat / flare / coal / quench, the eased
    /// display temperatures, the rainbow kitty momentum spine, the flame +
    /// specular phases, the typing cadence clock): the full-dark half that
    /// master-off and [`Self::reset`] add on top of
    /// [`Self::clear_transient_state`]. The zero-amplitude path does not call
    /// this: a momentary unfocus only cools by the elapsed gap, so earned
    /// forge momentum survives a focus blip.
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
        // The erase metric is a thermal integrator too: reset in the
        // full-dark wipe, not on the zero-amplitude path.
        self.erase_mom.reset();
        self.last_type = None;
    }

    /// Typing cadence → the thermal integrators (heat / coal / the canonical
    /// typing-momentum metric), plus the quench-steam flash on a deletion
    /// echo. Returns the observed inter-key `gap` (`f32::MAX` off the typing
    /// path), which the life/chain math downstream keys on.
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
        // Typing cadence → heat: each keystroke earns credit by how quickly
        // it followed the previous one, so only sustained fast typing ramps
        // the wake up; one quick correction doesn't flare. The life math
        // below also keys on `gap` (typing-continuity chaining).
        if typing {
            let gap = self
                .last_type
                .map(|t| now.saturating_duration_since(t).as_secs_f32())
                .unwrap_or(f32::MAX);
            // Forward-only momentum is a fire law ("sustained forward
            // momentum heats it"); the other styles keep direction-blind heat.
            if (forward || !fire) && !deletion && !navigation {
                // Shared helpers: the key-time click reconstructs this ramp to
                // time-align its timbre with the light (`cue_keystroke`).
                let cadence = Self::heat_cadence(gap);
                let gain = Self::heat_gain(cfg, fire);
                self.heat = (self.heat + cadence * gain).min(1.0);
                // The coal bed charges on a much wider cadence window: human
                // rhythm writing (300-400 ms/key) earns most of its credit, so
                // the bed builds a persistent ember floor over a sentence or
                // two. Fire-only.
                if fire {
                    let coal_cred = 1.0
                        - ((gap - Self::COAL_GAP_FULL)
                            / (Self::COAL_GAP_ZERO - Self::COAL_GAP_FULL))
                            .clamp(0.0, 1.0);
                    self.coal = (self.coal + coal_cred * Self::COAL_GAIN).min(1.0);
                }
            }
            // The canonical typing-momentum metric builds here and only here:
            // a non-delete printable typing advance (a forward glyph echo, a
            // typing wrap/re-anchor, or a coalesced multi-glyph echo).
            // Backward/vertical scrubbing, deletion echoes and navigation add
            // nothing; deletes and kills drain at their key hints
            // ([`Self::note_backspace`]/[`Self::note_kill`]). Rate
            // normalization lives in [`TypingMomentum::advance`], so key count
            // never buys momentum. Style-independent (a mid-run switch to
            // rainbow kitty arrives with its earned warmth).
            //
            // Typed correlation (`typed_pair`): a forward same-row echo alone
            // is program output, so every arm requires the fresh press hint
            // ([`Self::note_typed`], armed by the host on a real printable
            // key); neither PTY output nor a non-echoing keystream can buy the
            // cat or the stars. A printable key whose echo is a forward move
            // (vim `l`/`w` in normal mode) is indistinguishable from typed
            // text and is not mode-detected. The spine advances on the press
            // hint, not on candidate admission, so streaming collisions that
            // deny every candidate still let real typing earn its starfield. A
            // denied backspace landing wearing a wrap shape (`bs_pair`) is
            // excluded, and one press credits at most one advance (the hint
            // is consumed). The pulse below mirrors this advance onto the
            // cursor cat ([`Self::take_momentum_pulse`]).
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
                // A held park's fold, flushed on this tick ahead of the key
                // that followed it ([`HeldPark`]), keeps the tick's pulse: the
                // cat turns the corner it really turned.
                let fold_held = kind == CursorCatMotionKind::Advance
                    && self
                        .momentum_pulse
                        .is_some_and(|p| p.kind == CursorCatMotionKind::FoldForward);
                if !fold_held {
                    self.momentum_pulse = Some(CursorCatMotionPulse { at: now, kind });
                }
            } else if deletion && re_anchor && cc > pc && !navigation {
                // The input seam already delivered `CursorCat::on_key(false)`
                // for this Backspace; this pulse carries only the place change,
                // so momentum is not drained twice.
                self.momentum_pulse = Some(CursorCatMotionPulse {
                    at: now,
                    kind: CursorCatMotionKind::FoldReverse,
                });
            }
            self.last_type = Some(now);
            // Quench steam: a deletion echo flashes vapor off the doused cell,
            // scaling with the quench meter, plus a couple of hiss specks.
            if deletion && fire {
                let cell = geom.ch as f32;
                let ox = geom.origin_x as f32 + (pc as f32 + 0.5) * geom.cw as f32;
                let oy = geom.origin_y as f32 + (pr as f32 + 0.2) * cell;
                let puffs = (2 + (self.quench * 4.0) as usize).min(6);
                self.spawn_quench_steam(puffs, |_| ox, oy, cell, geom.cw as f32, now);
            }
            gap
        } else {
            f32::MAX
        }
    }
}
