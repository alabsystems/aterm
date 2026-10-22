// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Companion lifecycle: what the engine hands the cursor cat, the flying
//! head and the resident pet (momentum, motion pulses, impulses, the pet's
//! offer and the celebration drive).

use super::*;

/// The cursor movement the authenticated typing classifier offers the classic
/// flying kitty. A fold is one typed character whose projection crossed a line
/// boundary (or re-anchored an input box), not licence to fly across every
/// intervening cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorCatMotionKind {
    /// An ordinary forward/coalesced typed advance. Builds kitty momentum but
    /// starts no placement seam.
    Advance,
    /// A forward line fold: right edge to the following line's left edge, or
    /// the same shape after bottom-row scrolling / bottom-anchored box growth.
    FoldForward,
    /// The exact Backspace-owned inverse: left edge back to the preceding
    /// visual line's right edge (including a same-row reflow projection).
    FoldReverse,
}

/// One per-tick cursor-cat motion verdict, carrying the classifier's injected
/// clock so the cat's placement transition is wall-time deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorCatMotionPulse {
    pub at: Instant,
    pub kind: CursorCatMotionKind,
}

impl CursorCatMotionPulse {
    /// Whether this pulse is one of the forward advances that must be mirrored
    /// into [`crate::kitty_cursor::CursorCat`]'s canonical momentum instance.
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
    /// The erase-momentum mirror of [`Self::typing_momentum`] at `now`: how
    /// hard a delete run is going, on the same law. Drives the poof's energy
    /// (`erase_poof_drive`).
    #[inline]
    #[must_use]
    pub fn erase_momentum(&self, now: Instant) -> f32 {
        self.erase_mom.value(now)
    }

    /// The canonical typing-momentum metric at `now` ([`crate::typing_momentum`])
    /// every Rainbow Kitty "earned drama" consumer keys off; the same metric as
    /// [`crate::kitty_cursor::CursorCat::momentum`].
    #[inline]
    #[must_use]
    pub fn typing_momentum(&self, now: Instant) -> f32 {
        self.momentum.value(now)
    }

    /// Read and clear this tick's momentum pulse: `Some(instant)` iff the
    /// ribbon took one correlated typed advance this tick (the gate in
    /// [`Self::spawn`]), so the cat and the ribbon build momentum from one
    /// echo-correlated source; a non-echoing keystream pulses neither.
    #[must_use]
    #[cfg(test)]
    pub const fn take_momentum_pulse(&mut self) -> Option<Instant> {
        match self.momentum_pulse.take() {
            Some(pulse) if pulse.advances_momentum() => Some(pulse.at),
            _ => None,
        }
    }

    /// Read and clear this tick's cursor-cat motion pulse, which keeps the
    /// authenticated fold shape so the placement layer never infers a wrap from
    /// a large cursor relocation.
    #[inline]
    #[must_use]
    pub const fn take_cursor_cat_motion_pulse(&mut self) -> Option<CursorCatMotionPulse> {
        self.momentum_pulse.take()
    }

    /// Seam point 10 (`RAINBOW-KITTY-V2.md` §17.2, §7.2): read and clear the
    /// one companion impulse v2 minted (the flying head's teleport + whip on
    /// a meteor, the reduced-motion `Land`, the erase/kill `Wince`, the
    /// earned `Delight`). Whichever body owns the frame consumes it
    /// ([`rk::companion`]). `None` whenever v2 is not engaged; the stride
    /// pulse still comes from [`Self::take_cursor_cat_motion_pulse`].
    #[inline]
    #[must_use]
    pub const fn take_companion_impulse(&mut self) -> Option<CompanionImpulse> {
        if self.v2.engaged() {
            self.v2.take_companion_impulse()
        } else {
            None
        }
    }

    /// The resident pet's offer: the perk edge, the star within reach and the
    /// ribbon's colour under the cat, for
    /// [`crate::kitty_pet::PetBrain::note_v2_offer`]. A pure read; call it
    /// before [`Self::take_companion_impulse`] drains the frame's impulse,
    /// where the perk edge lives. `PetOffer::default()` unless v2 is engaged.
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

    /// The cat caught the star: spend the offered star's remaining life on the
    /// sky's own 40 ms finish. `star` is what
    /// [`crate::kitty_pet::PetBrain::note_v2_offer`] handed back and `at` the
    /// paw's landing instant. Returns whether the star was still there; inert,
    /// and `false`, unless v2 is engaged. Nothing is drawn.
    #[inline]
    pub fn catch_star(&mut self, star: rk::companion::StarCatch, at: Instant) -> bool {
        self.v2.engaged() && self.v2.catch_star(star, at)
    }

    /// The kitty's Delight edge (§5.8, §7.2): mints [`CompanionImpulse::Delight`]
    /// and buffers the earned m1 hero at `(row, col)` for the next engaged tick.
    #[inline]
    pub fn earn_hero(&mut self, now: Instant, row: u16, col: u16) {
        if self.v2.engaged() {
            self.unsettle();
            self.v2.earn_hero(now, row, col);
        }
    }

    /// The verdict's hot resume: the host reports that a shell command came
    /// back green after a long run, on the OSC 133/633 `D` it already
    /// dedupes. Not an [`rk::Event`]; inert unless v2 is engaged, and mints
    /// no light: it only prices the next key typed
    /// ([`rk::spine::Spine::note_verdict`]).
    #[inline]
    pub const fn note_command_verdict(&mut self, now: Instant) {
        if self.v2.engaged() {
            self.v2.note_verdict(now);
        }
    }

    /// The caret seam (§7.1): the instant a v2 meteor's frame-0 flare fired,
    /// while its spring-snap relax is live; `None` otherwise or when v2 is not
    /// engaged. The host copies it into
    /// [`crate::cursor_rainbow::RainbowConfig::flare_at`].
    #[inline]
    #[must_use]
    pub const fn caret_flare_at(&self) -> Option<Instant> {
        if self.v2.engaged() {
            self.v2_caret.flare_at
        } else {
            None
        }
    }

    /// The caret's paint at `now` for `RainbowConfig::paint` (§7.1): under v2
    /// the display spine floored by the landing re-light
    /// ([`rk::Engine::caret_paint`]); `0.0` when v2 is not engaged.
    #[inline]
    #[must_use]
    pub fn caret_paint(&self, now: Instant) -> f32 {
        if self.v2.engaged() {
            self.v2.caret_paint(now)
        } else {
            0.0
        }
    }

    /// The pet's one-shot: `true` exactly once per flow entry (the drain on
    /// which [`rk::Flow::open`] first reads true), never on an exit; the host
    /// drains it once per frame into [`crate::kitty_pet::PetBrain::note_flow`].
    /// A run that opens, breaks and opens again is two entries.
    #[inline]
    pub fn take_flow_entry(&mut self) -> bool {
        let open = self.flow_status().open();
        let entered = open && !self.flow_open;
        self.flow_open = open;
        entered
    }

    /// Sing-along drive (`crate::kitty_sing`): pin the canonical metric to at
    /// least `drive`, called by the host once per frame while a celebration is
    /// live. A deliberate momentum bypass, the twin of
    /// [`crate::kitty_cursor::CursorCat::set_singing`]: every legibility cap
    /// holds at full drive as at earned full momentum. During wind-down
    /// (`drive < 1`) the floor eases down and natural decay takes over.
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
        // §7.2's celebration arm: the same bypass through v2's one follower.
        if self.v2.engaged() {
            self.v2.celebrate(now, drive);
        }
    }

    /// The party (RAINBOW-KITTY-V2.md §27): a sing-along bar's fan (`n`
    /// stars thrown ROYGBIV from the caret, the shockwave `ring` on every
    /// fourth bar) or the armed celebration's drop fan. Reaches v2 only;
    /// every other style is unaffected.
    #[inline]
    pub const fn party(&mut self, now: Instant, n: u8, ring: bool) {
        if self.v2.engaged() {
            self.v2.party(now, n, ring);
        }
    }

    /// The grief gate, [`Self::celebrate`]'s inverse: while the pet's failure
    /// droop is on glass (`kitty_pet::PetBrain::grieving`) the host calls this
    /// every frame, so zeroed momentum defuels the jump meteor's star shower
    /// and the prompt after a failed command never arrives under a celebration.
    pub fn hush_fanfare(&mut self, now: Instant) {
        self.unsettle();
        self.momentum.set_value(now, 0.0);
        self.flow_open = false;
        if self.v2.engaged() {
            self.v2.hush_fanfare(now);
        }
    }
}
