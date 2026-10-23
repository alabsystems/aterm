// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sound cues: the key-time click ledger and the cues a move or a key mints for the host's synth.

use super::*;

/// One recorded sound trigger, the aural twin of a visual spawn, drained by the host after [`CursorGlow::tick`] and mapped onto a [`crate::trail_sound::SoundEvent`].
#[derive(Clone, Copy, Debug)]
pub struct SoundCue {
    pub kind: crate::trail_sound::SoundKind,
    /// The arrival column (grid cells) — the host maps it to stereo pan.
    pub col: u16,
    /// The blaze 0..1 at the moment of the move (typing heat / jump flare).
    pub heat: f32,
    /// The live sweep hue 0..1 (phaser's laid-hue band; other styles ignore).
    pub hue: f32,
    /// Cursor-movement direction: +1 rightward/upward, -1 left/down, 0 for gestures with no travel (typed, jump, backspace, kill).
    pub dir: i8,
    /// The glyph was shifted (a capital or shifted symbol), so the keystroke sounds an octave up ([`crate::trail_sound::SoundEvent::shifted`]).
    pub shifted: bool,
}

impl CursorGlow {
    /// Sound-cue backlog cap.
    pub(super) const MAX_SOUND_CUES: usize = 8;

    /// Record one sound cue (drops when the backlog is full).
    #[inline]
    pub(super) fn cue_sound(&mut self, kind: crate::trail_sound::SoundKind, col: u16) {
        self.cue_sound_at(kind, col, self.blaze());
    }

    /// [`Self::cue_sound`] with the blaze supplied rather than sampled, for the key-time click.
    #[inline]
    pub(super) fn cue_sound_at(
        &mut self,
        kind: crate::trail_sound::SoundKind,
        col: u16,
        heat: f32,
    ) {
        self.cue_sound_shifted(kind, col, heat, false);
    }

    /// [`Self::cue_sound_at`] with shiftedness supplied, for the key-time seam.
    #[inline]
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
    pub(super) const KEYED_CLICK_FRESH: f32 = 0.75;

    /// Ceiling on unspent key-time click credits, the cue backlog's bound: no echo run can outnumber the cues it could produce.
    pub(super) const MAX_KEYED_CLICKS: u8 = Self::MAX_SOUND_CUES as u8;

    /// Host key seam: click one typed glyph at the physical keypress rather than at its echo, which can arrive hundreds of ms late (a click detaches from its key past ~20 ms).
    #[inline]
    pub fn cue_keystroke(&mut self, now: Instant) -> bool {
        self.cue_keystroke_kind(now, crate::trail_sound::SoundKind::Typed)
    }

    /// [`Self::cue_keystroke`] with the gesture named by the host: the spacebar's comma ([`trail_sound::SoundKind::Space`]) rides here, since only the host knows the key.
    #[inline]
    pub fn cue_keystroke_kind(
        &mut self,
        now: Instant,
        kind: crate::trail_sound::SoundKind,
    ) -> bool {
        self.cue_keystroke_shifted(now, kind, false)
    }

    /// [`Self::cue_keystroke_kind`] with shiftedness from the host, for the capital's octave lift ([`SoundCue::shifted`]); only the host has the modifier flags.
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

    /// The bare-Shift lift cue ([`trail_sound::SoundKind::Shift`]) for a press with no echo: it banks no echo-muting credit and stamps no typing-cadence clock ([`Self::key_cue_at`] is for keystrokes).
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

    /// Host paste seam (`docs/design/RAINBOW-KITTY-V2.md` §28): a paste landed, cue its one up-strum ([`trail_sound::SoundKind::Strum`]), for a human's Cmd-V and an agent's `paste` / `turn` alike.
    #[inline]
    pub fn cue_paste(&mut self, now: Instant) -> bool {
        if !self.v2.engaged() {
            return false;
        }
        self.cue_keystroke_kind(now, crate::trail_sound::SoundKind::Strum)
    }

    /// The blaze a key-time click carries: the standing heat/flare cooled to `now` as the next tick's lazy decay will, plus the cadence credit this keystroke's echo is about to add.
    #[inline]
    pub(super) fn keyed_blaze(&self, now: Instant) -> f32 {
        let (heat, flare) = self.cooled_blaze(now);
        let gap = self
            .key_cue_at
            .map_or(f32::MAX, |t| now.saturating_duration_since(t).as_secs_f32());
        let charged = (heat + Self::heat_cadence(gap) * self.heat_gain_live).min(1.0);
        charged.max(flare).clamp(0.0, 1.0)
    }

    /// The standing heat/flare cooled to `now` as the next tick will cool them: [`Self::keyed_blaze`] without the cadence charge, for a cue whose press earns no echo (the Shift lift).
    #[inline]
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

    /// Spend one fresh key-time click credit: `true` means this echo's click already played at the key.
    #[inline]
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

    /// Drop every unspent key-time click credit, alongside clearing [`Self::sound_cues`], so no credit mutes the first click after the aurora returns.
    #[inline]
    pub(super) const fn clear_keyed_clicks(&mut self) {
        self.keyed_clicks = 0;
        self.keyed_click_at = None;
    }

    /// Cursor-movement threshold (chebyshev cells): an admitted move at least this long is a Sweep, a shorter one a Glide.
    pub(super) const SWEEP_MIN_DIST: f32 = 2.0;

    /// Record one cursor-movement cue (Glide/Sweep) with its travel direction, carried inside `kind` for the synth and on `dir` for the host.
    #[inline]
    pub(super) fn cue_cursor(&mut self, kind: crate::trail_sound::SoundKind, col: u16, dir: i8) {
        if self.sound_cues.len() < Self::MAX_SOUND_CUES {
            self.sound_cues.push(SoundCue {
                kind,
                col,
                heat: self.blaze(),
                hue: self.hue,
                dir,
                shifted: false,
            });
        }
    }

    /// Drain the sound cues recorded since the last call (the host feeds them to the synth after [`Self::tick`]).
    #[inline]
    pub fn drain_sound_cues(&mut self) -> std::vec::Drain<'_, SoundCue> {
        // v2 flushes its cues into this backlog inside `tick`; appending its
        // drain here leaves no cue behind on a non-ticking path.
        if self.v2.engaged() {
            self.sound_cues.extend(self.v2.drain_sound_cues());
        }
        self.sound_cues.drain(..)
    }

    /// Take back the cue [`Self::cue_keystroke`] just recorded, so the host can hand it to the synth at the key rather than at the next [`Self::drain_sound_cues`].
    #[inline]
    pub fn take_key_cue(&mut self) -> Option<SoundCue> {
        self.sound_cues.pop()
    }

    /// Drop the sound ledger: pending cues and the keyed-click credits for them.
    #[inline]
    pub(super) fn clear_sound_ledger(&mut self) {
        self.sound_cues.clear();
        self.v2_cues.clear();
        let _ = self.v2.drain_sound_cues();
        self.clear_keyed_clicks();
    }

    /// Snapshot the key-time click's timbre inputs ([`Self::heat_gain_live`], [`Self::heat_tau_live`]) from this tick's config (pure functions of `cfg` that never read `intensity`).
    #[inline]
    pub(super) const fn store_key_timbre(&mut self, cfg: &GlowConfig) {
        self.heat_gain_live = Self::heat_gain(matches!(cfg.style, GlowStyle::Fire));
        self.heat_tau_live = Self::heat_tau(cfg);
    }
}
