// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! COMPANION LIFECYCLE — what the engine hands the cursor cat, the flying
//! head and the resident pet: momentum, motion pulses, impulses, the pet's
//! offer and the celebration drive.

use super::*;

/// The exact cursor movement the authenticated typing classifier offers to the
/// classic flying kitty. A fold is deliberately distinct from a large jump:
/// it is one authored character of movement whose terminal projection crossed
/// a line boundary (or re-anchored an input box), not permission to fly across
/// every intervening cell.
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
    pub fn advances_momentum(self) -> bool {
        matches!(
            self.kind,
            CursorCatMotionKind::Advance | CursorCatMotionKind::FoldForward
        )
    }
}

impl CursorGlow {
    /// The ERASE-momentum mirror of [`Self::typing_momentum`] at `now` — how
    /// hard a delete run is going, on the identical law. Drives the poof's
    /// energy (see `erase_poof_drive`); exposed so hosts/tests can observe it.
    #[must_use]
    pub fn erase_momentum(&self, now: Instant) -> f32 {
        self.erase_mom.value(now)
    }

    /// THE canonical typing-momentum metric at `now`
    /// ([`crate::typing_momentum`]) — the one number every rainbow kitty "earned
    /// drama" consumer keys off (the eased `rainbow.disp` spine chases exactly
    /// this). Exposed so hosts/tests can observe the unified metric; the
    /// unified-readers proof (`momentum_unifies_glow_and_cat_metrics`) pins
    /// it against [`crate::kitty_cursor::CursorCat::momentum`].
    #[must_use]
    pub fn typing_momentum(&self, now: Instant) -> f32 {
        self.momentum.value(now)
    }

    /// Read and CLEAR this tick's momentum pulse: `Some(instant)` iff the
    /// ribbon took one correlated typed advance during the tick just run
    /// (a real printable keystroke paired with its forward / wrap / coalesced
    /// echo — the exact "earned by real typing only" gate in [`Self::spawn`]).
    /// The host feeds this instant into the cursor cat's own metric
    /// ([`crate::kitty_cursor::CursorCat::on_key`] with `forward = true`) so the
    /// cat and the ribbon build momentum from ONE echo-correlated source and
    /// cannot diverge — a key-only, non-echoing keystream (a password prompt,
    /// vim vertical navigation) pulses on neither.
    #[must_use]
    #[cfg(test)]
    pub fn take_momentum_pulse(&mut self) -> Option<Instant> {
        self.momentum_pulse
            .take()
            .filter(|pulse| pulse.advances_momentum())
            .map(|pulse| pulse.at)
    }

    /// Read and CLEAR the lossless cursor-cat motion pulse for this tick.
    /// Unlike `Self::take_momentum_pulse`, this preserves the authenticated
    /// forward/reverse fold shape so the placement layer never has to infer a
    /// wrap from an arbitrary large cursor relocation.
    #[must_use]
    pub fn take_cursor_cat_motion_pulse(&mut self) -> Option<CursorCatMotionPulse> {
        self.momentum_pulse.take()
    }

    /// **SEAM POINT 10** (`RAINBOW-KITTY-V2.md` §17.2, §7.2, D13): read and
    /// CLEAR the ONE companion impulse v2 minted — the flying head's
    /// teleport + whip on a meteor, the reduced-motion `Land`, the
    /// erase/kill `Wince`, the earned `Delight`. Whichever body owns the
    /// frame consumes it ([`rk::companion`]). `None` whenever v2 is not
    /// engaged, so a host that wires this beside
    /// [`Self::take_cursor_cat_motion_pulse`] — which keeps carrying v1's
    /// authenticated stride pulse, so the kitty's momentum is still built
    /// from the one echo-correlated source — pays one bool on every other
    /// style.
    #[must_use]
    pub fn take_companion_impulse(&mut self) -> Option<CompanionImpulse> {
        if self.v2.engaged() {
            self.v2.take_companion_impulse()
        } else {
            None
        }
    }

    /// **THE RESIDENT PET'S OFFER** (`rk::companion` panel #10, D13) — the
    /// perk edge, the star within reach and the ribbon's colour under the
    /// cat, as ONE value, for the host to hand to
    /// [`crate::kitty_pet::PetBrain::note_v2_offer`].
    ///
    /// A PURE READ that arms nothing: every field is state a producer already
    /// owns this frame, so an idle engine offers `PetOffer::default()` and no
    /// clock, mark or cadence moves. Call it BEFORE
    /// [`Self::take_companion_impulse`] drains the frame's impulse — the perk
    /// edge lives in that slot, and a drained slot offers none.
    /// `PetOffer::default()` whenever v2 is not engaged, so a host that wires
    /// this beside the pet's own tick pays one bool on every other style.
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

    /// **THE CAT CAUGHT THE STAR** (panel #10(b)) — spend the offered star's
    /// remaining life on the sky's own 40 ms finish. `star` is exactly the
    /// value [`crate::kitty_pet::PetBrain::note_v2_offer`] handed back and
    /// `at` is the PAW'S LANDING instant. Returns whether the star was still
    /// there to catch; inert, and `false`, unless v2 is engaged.
    ///
    /// Nothing is drawn: the whole of the catch is one star ending sooner,
    /// which is the only way the resident's own motion may touch the sky
    /// without becoming light itself (T1).
    pub fn catch_star(&mut self, star: rk::companion::StarCatch, at: Instant) -> bool {
        self.v2.engaged() && self.v2.catch_star(star, at)
    }

    /// **THE VERDICT'S HOT RESUME** (THE VERDICT, sense 3): the host reports
    /// that a shell command came back GREEN after a long run, on the OSC
    /// 133/633 `D` it already dedupes. Not a cursor event, so it is not an
    /// [`rk::Event`]; inert unless v2 is engaged, and inert on glass even when
    /// it is. It mints no
    /// light: it only prices the first key you type next
    /// ([`rk::spine::Spine::note_verdict`]).
    pub fn note_command_verdict(&mut self, now: Instant) {
        if self.v2.engaged() {
            self.v2.note_verdict(now);
        }
    }

    /// **THE CARET SEAM** (§7.1): the instant a v2 meteor's frame-0 flare
    /// fired, while its spring-snap relax is live — `None` otherwise, and
    /// always `None` when v2 is not engaged. The host copies it into
    /// [`crate::cursor_rainbow::RainbowConfig::flare_at`] where it builds the
    /// caret's config; v2 does not own the block fill.
    #[must_use]
    pub fn caret_flare_at(&self) -> Option<Instant> {
        if self.v2.engaged() {
            self.v2_caret.flare_at
        } else {
            None
        }
    }

    /// The caret's PAINT at `now` for `RainbowConfig::paint` (§7.1): under
    /// v2 the display spine floored by the landing re-light
    /// ([`rk::Engine::caret_paint`] — the caret is never dimmed at the
    /// destination); otherwise exactly [`Self::momentum_display`], so a host
    /// that moves its `paint` feed to this read changes nothing for v1.
    #[must_use]
    pub fn caret_paint(&self, now: Instant) -> f32 {
        if self.v2.engaged() {
            self.v2.caret_paint(now)
        } else {
            0.0
        }
    }

    /// **THE PET'S ONE-SHOT** — `true` exactly once per flow ENTRY (the drain
    /// on which [`rk::Flow::open`] first reads true), and never on an exit.
    ///
    /// The host drains it once per frame into
    /// [`crate::kitty_pet::PetBrain::note_flow`]. An EDGE over the engine's
    /// counter, not a counter of its own: one `bool` of memo, so a run that
    /// opens, breaks and opens again is two entries and a run that merely
    /// continues is none. A latch, not a light — it asks for no frame, spawns
    /// nothing, and a drain nobody forwards costs one compare.
    pub fn take_flow_entry(&mut self) -> bool {
        let open = self.flow_status().open();
        let entered = open && !self.flow_open;
        self.flow_open = open;
        entered
    }

    /// SING-ALONG drive (`crate::kitty_sing`): pin the canonical
    /// metric to at least `drive`, called by the host once per frame while a
    /// celebration is live.
    ///
    /// THE MOMENTUM BYPASS (documented, deliberate — the twin of
    /// [`crate::kitty_cursor::CursorCat::set_singing`]'s): an ARMED held-key
    /// celebration IS maximal flow BY DEFINITION, so the metric that was
    /// rate-normalized precisely to stop key-repeat floods from out-earning
    /// typing is driven straight to 1.0 — full ribbon saturation, the
    /// maximal star shower, every "earned drama" consumer at its ceiling
    /// through the one existing spine (no parallel celebration code path in
    /// the render, so every LEGIBILITY CAP — star wash, occupied-cell
    /// coverage, contrast floors — holds at full drive exactly as it holds
    /// at genuinely earned full momentum;
    /// `full_sing_drive_holds_the_rainbow_legibility_caps` pins that). During
    /// wind-down (`drive < 1`) the floor eases down with the crossfade and
    /// natural decay takes over — never a hard cut. Uses the same `set_value` seam as the collection hello's
    /// "guaranteed visible at full momentum" arm.
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
        // §7.2's celebration arm, restated for v2: the SAME documented
        // momentum bypass, through its one follower, so every legibility cap
        // holds at full drive exactly as it holds at earned full momentum.
        if self.v2.engaged() {
            self.v2.celebrate(now, drive);
        }
    }

    /// **THE PARTY** (RAINBOW-KITTY-V2.md §27): a sing-along bar's fan — `n`
    /// stars thrown ROYGBIV from the caret, the shockwave `ring` on every
    /// fourth bar — or the armed celebration's drop fan. Reaches v2 only;
    /// every other style reads nothing here, so the nine stay byte-identical.
    pub fn party(&mut self, now: Instant, n: u8, ring: bool) {
        if self.v2.engaged() {
            self.v2.party(now, n, ring);
        }
    }

    /// The GRIEF GATE (0.19.0 gauntlet F4a) — [`Self::celebrate`]'s inverse.
    /// While the pet's failure droop is on glass
    /// (`kitty_pet::PetBrain::grieving`), the host calls this every frame:
    /// zeroing the rainbow momentum defuels the jump meteor's star shower
    /// and its terminus scatter, so the fresh prompt after a FAILED command
    /// never arrives under a celebration ring with '+' crosses landing on
    /// the very words that broke. Natural decay already heads to zero —
    /// this only refuses to let the failure's own prompt jump re-arm the
    /// party mid-grief; momentum re-earns normally once the droop ends.
    pub fn hush_fanfare(&mut self, now: Instant) {
        self.unsettle();
        self.momentum.set_value(now, 0.0);
    }
}
