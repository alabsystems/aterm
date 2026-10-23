// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Companion lifecycle: momentum, motion pulses, impulses, pet offers, and celebration drive.

use super::*;

/// Cursor movement classified by the authenticated typing classifier for the flying kitty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorCatMotionKind {
    /// Ordinary forward or coalesced typed advance.
    Advance,
    /// Forward line fold from the right edge to the following line's left edge.
    FoldForward,
    /// Backspace-owned inverse fold back to the preceding visual line's right edge.
    FoldReverse,
}

/// Per-tick cursor-cat motion verdict with the classifier's injected timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorCatMotionPulse {
    pub at: Instant,
    pub kind: CursorCatMotionKind,
}

impl CursorCatMotionPulse {
    /// Whether this pulse advances [`crate::kitty_cursor::CursorCat`]'s canonical momentum.
    #[must_use]
    #[cfg(test)]
    pub const fn advances_momentum(self) -> bool {
        matches!(
            self.kind,
            CursorCatMotionKind::Advance | CursorCatMotionKind::FoldForward
        )
    }
}

impl CursorGlow {
    /// Erase-momentum mirror of [`Self::typing_momentum`] at `now`.
    #[inline]
    #[must_use]
    pub fn erase_momentum(&self, now: Instant) -> f32 {
        self.erase_mom.value(now)
    }

    /// Canonical typing-momentum metric at `now` ([`crate::typing_momentum`]).
    #[inline]
    #[must_use]
    pub fn typing_momentum(&self, now: Instant) -> f32 {
        self.momentum.value(now)
    }

    /// Read and clear this tick's forward momentum pulse.
    #[must_use]
    #[cfg(test)]
    pub const fn take_momentum_pulse(&mut self) -> Option<Instant> {
        match self.momentum_pulse.take() {
            Some(pulse) if pulse.advances_momentum() => Some(pulse.at),
            _ => None,
        }
    }

    /// Read and clear this tick's cursor-cat motion pulse.
    #[inline]
    #[must_use]
    pub const fn take_cursor_cat_motion_pulse(&mut self) -> Option<CursorCatMotionPulse> {
        self.momentum_pulse.take()
    }

    /// Read and clear the v2 companion impulse ([`rk::companion`]), or `None` when v2 is not engaged.
    #[inline]
    #[must_use]
    pub const fn take_companion_impulse(&mut self) -> Option<CompanionImpulse> {
        if self.v2.engaged() {
            self.v2.take_companion_impulse()
        } else {
            None
        }
    }

    /// Read the resident pet's offer for [`crate::kitty_pet::PetBrain::note_v2_offer`].
    #[inline]
    #[must_use]
    pub fn pet_offer(
        &self,
        geom: Geom,
        pet: Option<rk::companion::PetOnGlass>,
    ) -> rk::companion::PetOffer {
        if self.v2.engaged() {
            self.v2.pet_offer(geom, pet)
        } else {
            rk::companion::PetOffer::default()
        }
    }

    /// Spend the offered star's remaining life on the 40 ms catch finish when v2 is engaged.
    #[inline]
    pub fn catch_star(&mut self, star: rk::companion::StarCatch, at: Instant) -> bool {
        self.v2.engaged() && self.v2.catch_star(star, at)
    }

    /// The pet's drawn box this frame, window pixels (`None`: off glass), so
    /// the sky draws no star on its body ([`rk::companion::PetShade`]).
    #[inline]
    pub const fn note_pet_body(&mut self, body: Option<(i32, i32, i32, i32)>) {
        self.v2.note_pet_body(body);
    }

    /// The pet's body box the sky last drew no star on, window pixels
    /// (`None`: off glass, or no frame offered one).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn pet_shade(&self) -> Option<(i32, i32, i32, i32)> {
        self.v2.pet_shade()
    }

    /// Mint [`CompanionImpulse::Delight`] and buffer the earned m1 hero at `(row, col)`.
    #[inline]
    pub fn earn_hero(&mut self, now: Instant, row: u16, col: u16) {
        if self.v2.engaged() {
            self.unsettle();
            self.v2.earn_hero(now, row, col);
        }
    }

    /// Record a green long-running command verdict to price the next key ([`rk::spine::Spine::note_verdict`]).
    #[inline]
    pub const fn note_command_verdict(&mut self, now: Instant) {
        if self.v2.engaged() {
            self.v2.note_verdict(now);
        }
    }

    /// Instant a v2 meteor's frame-0 flare fired while its relax is live, for [`crate::cursor_rainbow::RainbowConfig::flare_at`].
    #[inline]
    #[must_use]
    pub const fn caret_flare_at(&self) -> Option<Instant> {
        if self.v2.engaged() {
            self.v2_caret.flare_at
        } else {
            None
        }
    }

    /// Caret paint at `now` ([`rk::Engine::caret_paint`]), or `0.0` when v2 is not engaged.
    #[inline]
    #[must_use]
    pub fn caret_paint(&self, now: Instant) -> f32 {
        if self.v2.engaged() {
            self.v2.caret_paint(now)
        } else {
            0.0
        }
    }

    /// Return `true` once per flow entry into [`rk::Flow::open`] for [`crate::kitty_pet::PetBrain::note_flow`].
    #[inline]
    pub fn take_flow_entry(&mut self) -> bool {
        let open = self.flow_status().open();
        let entered = open && !self.flow_open;
        self.flow_open = open;
        entered
    }

    /// Pin typing momentum to at least `drive` during a live celebration ([`crate::kitty_sing`]).
    pub fn celebrate(&mut self, now: Instant, drive: f32) {
        self.unsettle();
        let drive = if drive.is_finite() {
            drive.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if self.momentum.value(now) < drive {
            self.momentum.set_value(now, drive);
        }
        if self.v2.engaged() {
            self.v2.celebrate(now, drive);
        }
    }

    /// Trigger a v2 sing-along bar fan (`n` stars and optional shockwave `ring`).
    #[inline]
    pub const fn party(&mut self, now: Instant, n: u8, ring: bool) {
        if self.v2.engaged() {
            self.v2.party(now, n, ring);
        }
    }

    /// Zero typing momentum and close flow while the pet is grieving.
    pub fn hush_fanfare(&mut self, now: Instant) {
        self.unsettle();
        self.momentum.set_value(now, 0.0);
        self.flow_open = false;
        if self.v2.engaged() {
            self.v2.hush_fanfare(now);
        }
    }
}
