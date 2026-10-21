// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sound cues: the key-time click ledger and the cues a move or a key mints
//! for the host's synth.

use super::*;

/// One recorded sound trigger, the aural twin of a visual spawn, drained by the
/// host after [`CursorGlow::tick`] and mapped onto a
/// [`crate::trail_sound::SoundEvent`]. The host owns style, focus/motion gating,
/// volume and pan; the engine owns the timing and the classification.
#[derive(Clone, Copy, Debug)]
pub struct SoundCue {
    pub kind: crate::trail_sound::SoundKind,
    /// The arrival column (grid cells) — the host maps it to stereo pan.
    pub col: u16,
    /// The blaze 0..1 at the moment of the move (typing heat / jump flare).
    pub heat: f32,
    /// The live sweep hue 0..1 (phaser's laid-hue band; other styles ignore).
    pub hue: f32,
    /// Cursor-movement direction: +1 rightward/upward, -1 left/down, 0 for
    /// gestures with no travel (typed, jump, backspace, kill). A Glide/Sweep
    /// also carries it inside `kind`, so the host forwards it unmapped; this
    /// field is the explicit host-facing copy.
    pub dir: i8,
    /// The glyph was shifted (a capital or shifted symbol), forwarded to
    /// [`crate::trail_sound::SoundEvent::shifted`] so the keystroke sounds an
    /// octave up (owner, 2026-08-30).
    ///
    /// Only the key-time seam sets it ([`CursorGlow::cue_keystroke_shifted`]),
    /// since shiftedness lives in the key event's modifiers. Every cue minted
    /// from observed cursor motion carries `false`: an echo has no key behind
    /// it.
    pub shifted: bool,
}

impl CursorGlow {
    /// Sound-cue backlog cap. Cues drain every host tick; the cap matters only
    /// on a stalled host (minimized), where replaying stale keystroke sounds
    /// would be noise, so overflow is dropped.
    pub(super) const MAX_SOUND_CUES: usize = 8;

    /// Record one sound cue (drops when the backlog is full). Non-cursor
    /// gestures carry no travel direction (`dir = 0`).
    pub(super) fn cue_sound(&mut self, kind: crate::trail_sound::SoundKind, col: u16) {
        self.cue_sound_at(kind, col, self.blaze());
    }

    /// [`Self::cue_sound`] with the blaze supplied rather than sampled, for the
    /// key-time click. Echo-born cues pass `blaze()`; only
    /// [`Self::cue_keystroke`] passes the heat its own keystroke is about to
    /// add, which `self.heat` does not carry until the echo arrives.
    pub(super) fn cue_sound_at(
        &mut self,
        kind: crate::trail_sound::SoundKind,
        col: u16,
        heat: f32,
    ) {
        self.cue_sound_shifted(kind, col, heat, false);
    }

    /// [`Self::cue_sound_at`] with shiftedness supplied, for the key-time seam.
    /// Every other caller goes through `cue_sound_at` and gets `false`.
    pub(super) fn cue_sound_shifted(
        &mut self,
        kind: crate::trail_sound::SoundKind,
        col: u16,
        heat: f32,
        shifted: bool,
    ) {
        if self.sound_cues.len() < Self::MAX_SOUND_CUES {
            self.sound_cues.push(SoundCue {
                kind,
                col,
                heat,
                hue: self.hue,
                dir: 0,
                shifted,
            });
        }
    }

    /// How long a key-time click credit stays spendable, from the newest key.
    /// It must cover the key→PTY→shell→PTY→echo round trip or the character
    /// clicks twice, and stay short or a swallowed key banks silence that eats a
    /// later output-driven click. 0.75 s covers local echo, a loaded flood frame
    /// and an intercontinental ssh hop (~300 ms) with margin.
    pub(super) const KEYED_CLICK_FRESH: f32 = 0.75;

    /// Ceiling on unspent key-time click credits, the cue backlog's bound: no
    /// echo run can outnumber the cues it could produce.
    pub(super) const MAX_KEYED_CLICKS: u8 = Self::MAX_SOUND_CUES as u8;

    /// Host key seam: click one typed glyph at the physical keypress instead of
    /// at its echo. Returns whether a cue was recorded (the host then requests
    /// the redraw that drains it; the drain runs ahead of the present early-out,
    /// so a pixel-identical frame still delivers the click).
    ///
    /// A cue born at the spawn edge arrives after key → PTY → shell → PTY →
    /// parse → next frame, tens to hundreds of ms late on a flooded session or
    /// remote link, well past the ~20 ms at which a click stops feeling attached
    /// to the key.
    ///
    /// Silence law: gated on [`Self::sound_live`] as of the last tick (master
    /// switch, real geometry, and the host's `audible` verdict, which only
    /// unfocus closes; reduced motion and load shed dim the light but keep the
    /// seam). A host that never ticks records nothing. Focus, the sound knob,
    /// volume and the resize quiet window are host policy at the drain.
    ///
    /// The cue's column is the last known (pre-echo) cursor cell, a cell left of
    /// where the echo would pan it; that is inaudible.
    ///
    /// Timbre: the cue carries the heat this keystroke is about to produce. Heat
    /// scales the synth's level as `0.55 + 0.45·heat` and its ember layer as
    /// `0.1 + 0.28·heat`, so one missing [`Self::HEAT_GAIN`] step is audible
    /// (about 0.8 dB). This
    /// cools the standing heat/flare by the gap since the last tick (`heat_at`)
    /// and adds the same cadence credit the echo will, off the key clock (under
    /// a flood, key intervals are the true rhythm). `self.heat` is not written:
    /// the light still charges at the echo, where the classification is known.
    ///
    /// `hue` is a free-running sweep phase with no keystroke term, so the
    /// key-time sample is the phase at which the click is heard.
    pub fn cue_keystroke(&mut self, now: Instant) -> bool {
        self.cue_keystroke_kind(now, crate::trail_sound::SoundKind::Typed)
    }

    /// [`Self::cue_keystroke`] with the gesture named by the host: the
    /// spacebar's comma ([`trail_sound::SoundKind::Space`]) rides here, since
    /// only the host knows the key. Silence law, backlog bound, column, timbre
    /// prediction and echo-muting credit are shared.
    pub fn cue_keystroke_kind(
        &mut self,
        now: Instant,
        kind: crate::trail_sound::SoundKind,
    ) -> bool {
        self.cue_keystroke_shifted(now, kind, false)
    }

    /// [`Self::cue_keystroke_kind`] with shiftedness from the host, for the
    /// capital's octave lift ([`SoundCue::shifted`]); only the host has the
    /// modifier flags. Unshifted is the identity.
    pub fn cue_keystroke_shifted(
        &mut self,
        now: Instant,
        kind: crate::trail_sound::SoundKind,
        shifted: bool,
    ) -> bool {
        // Dark ⇒ silent, by the same law the spawn edge gets for free.
        if !self.sound_live {
            return false;
        }
        // Backlog full: the host is not draining. Record nothing, not even a
        // credit, rather than bank a click that could only play stale.
        if self.sound_cues.len() >= Self::MAX_SOUND_CUES {
            return false;
        }
        let col = self
            .last
            .or_else(|| self.last_visible.map(|(c, _)| c))
            .map_or(0, |(_, c)| c);
        let heat = self.keyed_blaze(now);
        self.cue_sound_shifted(kind, col, heat, shifted);
        self.key_cue_at = Some(now);
        self.keyed_clicks = self
            .keyed_clicks
            .saturating_add(1)
            .min(Self::MAX_KEYED_CLICKS);
        self.keyed_click_at = Some(now);
        true
    }

    /// The bare-Shift lift cue ([`trail_sound::SoundKind::Shift`]) for a press
    /// with no echo: it banks no echo-muting credit and stamps no typing-cadence
    /// clock ([`Self::key_cue_at`] is for keystrokes). Same silence law and
    /// backlog bound as [`Self::cue_keystroke`]; the heat is the standing blaze
    /// cooled to `now` without a cadence charge. Take it with
    /// [`Self::take_key_cue`] right after a `true`.
    pub fn cue_modifier(&mut self, now: Instant) -> bool {
        if !self.sound_live {
            return false;
        }
        if self.sound_cues.len() >= Self::MAX_SOUND_CUES {
            return false;
        }
        let col = self
            .last
            .or_else(|| self.last_visible.map(|(c, _)| c))
            .map_or(0, |(_, c)| c);
        let (heat, flare) = self.cooled_blaze(now);
        self.cue_sound_at(
            crate::trail_sound::SoundKind::Shift,
            col,
            heat.max(flare).clamp(0.0, 1.0),
        );
        true
    }

    /// Host paste seam (`docs/design/RAINBOW-KITTY-V2.md` §28): a paste landed,
    /// cue its one up-strum ([`trail_sound::SoundKind::Strum`]). Keyed off the
    /// event kind at `App::input_paste`, so a human's Cmd-V and an agent's
    /// `paste` / `turn` cue alike. Returns whether a cue was recorded.
    ///
    /// Rainbow kitty only: other styles record and bank nothing, so their paste
    /// sounds as its echo's Jump. Under rainbow kitty it is the keystroke
    /// construction ([`Self::cue_keystroke_kind`]) including the echo credit, so
    /// the paste's echo stays silent and the strum is the gesture's one sound.
    pub fn cue_paste(&mut self, now: Instant) -> bool {
        if !self.v2.engaged() {
            return false;
        }
        self.cue_keystroke_kind(now, crate::trail_sound::SoundKind::Strum)
    }

    /// The blaze a key-time click carries: the standing heat/flare cooled to
    /// `now` as the next tick's lazy decay will, plus the cadence credit this
    /// keystroke's echo is about to add. Pure read (see [`Self::cue_keystroke`]).
    ///
    /// τ and gain come from the last drawing tick's snapshot (Fire's overrides
    /// included). A zero τ, unreachable behind `sound_live`,
    /// falls back to the standing heat instead of producing NaN.
    pub(super) fn keyed_blaze(&self, now: Instant) -> f32 {
        let (heat, flare) = self.cooled_blaze(now);
        let gap = self
            .key_cue_at
            .map_or(f32::MAX, |t| now.saturating_duration_since(t).as_secs_f32());
        let charged = (heat + Self::heat_cadence(gap) * self.heat_gain_live).min(1.0);
        charged.max(flare).clamp(0.0, 1.0)
    }

    /// The standing heat/flare cooled to `now` as the next tick will cool them:
    /// [`Self::keyed_blaze`] without the cadence charge, for a cue whose press
    /// earns no echo (the Shift lift). Pure read.
    pub(super) fn cooled_blaze(&self, now: Instant) -> (f32, f32) {
        let dt = self
            .heat_at
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
        if dt > 0.0 && self.heat_tau_live > 0.0 {
            (
                self.heat * (-dt / self.heat_tau_live).exp(),
                self.flare * (-dt / Self::FLARE_DECAY_TAU).exp(),
            )
        } else {
            (self.heat, self.flare)
        }
    }

    /// Spend one fresh key-time click credit: `true` means this echo's click
    /// already played at the key. Stale credits are dropped wholesale (see
    /// [`Self::KEYED_CLICK_FRESH`]).
    pub(super) fn take_keyed_click(&mut self, now: Instant) -> bool {
        if self.keyed_clicks == 0 {
            return false;
        }
        let fresh = self.keyed_click_at.is_some_and(|t| {
            now.saturating_duration_since(t).as_secs_f32() <= Self::KEYED_CLICK_FRESH
        });
        if !fresh {
            self.keyed_clicks = 0;
            self.keyed_click_at = None;
            return false;
        }
        self.keyed_clicks -= 1;
        if self.keyed_clicks == 0 {
            self.keyed_click_at = None;
        }
        true
    }

    /// Drop every unspent key-time click credit, alongside clearing
    /// [`Self::sound_cues`], so no credit mutes the first click after the
    /// aurora returns.
    pub(super) fn clear_keyed_clicks(&mut self) {
        self.keyed_clicks = 0;
        self.keyed_click_at = None;
    }

    /// Cursor-movement threshold (chebyshev cells): an admitted move at least
    /// this long is a Sweep, a shorter one a Glide. The universal candidate gate
    /// runs first.
    pub(super) const SWEEP_MIN_DIST: f32 = 2.0;

    /// Record one cursor-movement cue (Glide/Sweep) with its travel direction,
    /// carried inside `kind` for the synth and on `dir` for the host.
    pub(super) fn cue_cursor(&mut self, kind: crate::trail_sound::SoundKind, col: u16, dir: i8) {
        if self.sound_cues.len() < Self::MAX_SOUND_CUES {
            self.sound_cues.push(SoundCue {
                kind,
                col,
                heat: self.blaze(),
                hue: self.hue,
                dir,
                // A cursor motion is not a glyph: nothing was typed, so
                // nothing was shifted.
                shifted: false,
            });
        }
    }

    /// Drain the sound cues recorded since the last call (the host feeds them to
    /// the synth after [`Self::tick`]). Empty whenever the aurora is disabled,
    /// so a muted session does no sound work.
    pub fn drain_sound_cues(&mut self) -> std::vec::Drain<'_, SoundCue> {
        // v2 flushes its cues into this backlog inside `tick` (`rk::Frame::cues`),
        // so its own drain is empty between ticks; appending it here proves no
        // minted cue is left behind on a non-ticking path. `take_key_cue` is
        // unaffected: v2 mints nothing on a key edge.
        if self.v2.engaged() {
            self.sound_cues.extend(self.v2.drain_sound_cues());
        }
        self.sound_cues.drain(..)
    }

    /// Take back the cue [`Self::cue_keystroke`] just recorded, so the host can
    /// hand it to the synth at the key rather than at the next tick's
    /// [`Self::drain_sound_cues`]. Recording first runs the silence law, backlog
    /// bound, column sample and timbre prediction in one construction; this only
    /// changes who carries the cue.
    ///
    /// Valid only right after a `true` from `cue_keystroke`: it pops the most
    /// recent cue. Older cues stay queued for the frame drain.
    pub fn take_key_cue(&mut self) -> Option<SoundCue> {
        self.sound_cues.pop()
    }

    /// Drop the sound ledger: pending cues and the keyed-click credits for them.
    ///
    /// [`Self::reset`] and the master-off dark return call it unconditionally (a
    /// credit from a dead coordinate space must not mute the next click).
    /// [`Self::settle_key_seam`] (zero amplitude, degenerate grid, classic wake)
    /// calls it only when the tick is inaudible: a shed or motion-reduced frame
    /// is dark but heard, and its in-flight echoes must still find their credits
    /// or the burst double-clicks.
    ///
    /// Nothing visual belongs here: geometry, the candidate cohort, the held
    /// park, erase licences and the press ring still clear on every dark tick.
    pub(super) fn clear_sound_ledger(&mut self) {
        self.sound_cues.clear();
        self.clear_keyed_clicks();
    }

    /// Snapshot the key-time click's timbre inputs ([`Self::heat_gain_live`],
    /// [`Self::heat_tau_live`]) from this tick's config (pure functions of `cfg`
    /// that never read `intensity`). Every tick that may open the key seam calls
    /// this first.
    pub(super) fn store_key_timbre(&mut self, cfg: &GlowConfig) {
        self.heat_gain_live = Self::heat_gain(matches!(cfg.style, GlowStyle::Fire));
        self.heat_tau_live = Self::heat_tau(cfg);
    }
}
