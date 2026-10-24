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
        self.pet_body = body;
        self.v2.note_pet_body(body);
    }

    /// The pet's body box the sky last drew no star on, window pixels
    /// (`None`: off glass, or no frame offered one).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn pet_shade(&self) -> Option<(i32, i32, i32, i32)> {
        self.v2.pet_shade()
    }

    /// The boxes the sky and the ribbon last kept off, window pixels
    /// ([`Self::keep_out_boxes`]).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn keep_out(&self) -> &[(i32, i32, i32, i32)] {
        self.v2.keep_out()
    }

    /// The word icons the word pass drew (a `dog`'s or a `fox`'s face, a
    /// word's cat), grid pixels `(x0, x1, y0, y1)`: the next frame's sky and
    /// ribbon keep off them ([`rk::companion::KeepOut`]).
    #[inline]
    pub fn note_word_icons(&mut self, icons: impl IntoIterator<Item = (i32, i32, i32, i32)>) {
        self.word_icons.clear();
        self.word_icons.extend(icons);
        self.word_icon_src.clear();
        self.word_icons_unshaded = true;
    }

    /// This frame's [`rk::companion::KeepOut`] for the ribbon and its sky,
    /// window pixels: the word icons, and each run of lettered or filled
    /// cells (a ground that is not the page) of every row of the frame's
    /// text but the caret's.
    pub(super) fn keep_out_boxes(&mut self, cfg: &GlowConfig, geom: Geom, cur: Option<(u16, u16)>) {
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        self.keep_geom = Some((ox, oy, cw));
        let mut boxes = std::mem::take(&mut self.keep_out_scratch);
        boxes.clear();
        let page = cfg.theme_bg & 0x00FF_FFFF;
        let caret_row = cur.map(|(r, _)| i32::from(r));
        let ft = &self.frame_text;
        let mut rows = std::mem::take(&mut self.keep_rows_scratch);
        rows.clear();
        for i in 0..ft.rows.len() {
            let row = ft.lo + i as i32;
            if Some(row) == caret_row {
                continue;
            }
            let y0 = oy + row * ch;
            let row_pairs = ft
                .pairs
                .get(i * ft.cols..(i + 1) * ft.cols)
                .unwrap_or_default();
            let mut run: Option<i32> = None;
            let mut span: Option<(i32, i32)> = None;
            let end = ft.col0 + ft.cols as i32;
            for col in ft.col0..=end {
                let held = row_pairs
                    .get((col - ft.col0) as usize)
                    .copied()
                    .flatten()
                    .is_some_and(|(ink, ground)| {
                        ink.is_some() || !super::emit::reads_as_page(ground & 0x00FF_FFFF, page)
                    });
                match (held, run) {
                    (true, None) => run = Some(col),
                    (false, Some(c0)) => {
                        boxes.push((ox + c0 * cw, ox + col * cw, y0, y0 + ch));
                        span = Some(span.map_or((c0, col), |(a, _)| (a, col)));
                        run = None;
                    }
                    _ => {}
                }
            }
            // The span reaches a cell past the row's last letter, where its
            // caret stood when it was typed: a Return's train roots there,
            // and cut at the letters its root stood alone after the line as
            // a brown stub (looks6 round 3).
            if let Some((c0, c1)) = span {
                rows.push((ox + c0 * cw, ox + (c1 + 1) * cw, y0, y0 + ch));
            }
        }
        let icons = self
            .word_icons
            .iter()
            .map(|&(x0, x1, y0, y1)| (x0 + ox, x1 + ox, y0 + oy, y1 + oy));
        self.v2
            .note_keep_out((icons, boxes.iter().copied()), rows.iter().copied());
        self.keep_out_scratch = boxes;
        self.keep_rows_scratch = rows;
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
