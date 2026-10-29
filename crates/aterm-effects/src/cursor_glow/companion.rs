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
    #[inline]
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
    #[inline]
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
            rk::companion::PetOffer::EMPTY
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
        #[cfg(any(test, debug_assertions))]
        let body = match self.pet_body_pinned {
            Some(pinned) => Some(pinned),
            None => body,
        };
        self.pet_body = body;
        self.v2.note_pet_body(body);
    }

    /// The flying head's drawn body this frame, grid pixels `(x0, x1, y0, y1)`
    /// as the word pass's icons are ([`Self::note_word_icons`]); `None` when no
    /// head is on glass. Every frame's word pass opens with none.
    #[inline]
    pub const fn note_head_body(&mut self, body: Option<(i32, i32, i32, i32)>) {
        self.head_body = body;
    }

    /// The flying head's drawn body ([`Self::note_head_body`]), window pixels
    /// `(x0, x1, y0, y1)`: `trail status`'s `head_body=`, which the trail
    /// gallery keeps per frame for `lab trail-companion` (looks10 L10).
    #[inline]
    #[must_use]
    pub fn head_body_window_box(&self) -> Option<(i32, i32, i32, i32)> {
        let (ox, oy) = self.keep_geom.map_or((0, 0), |(ox, oy, _)| (ox, oy));
        self.head_body
            .map(|(x0, x1, y0, y1)| (x0 + ox, x1 + ox, y0 + oy, y1 + oy))
    }

    /// A test's stand-in pet: every frame's [`Self::note_pet_body`] notes
    /// `body` (window pixels) in place of the box its pet offered (`None`:
    /// the offered box), so a host's frames veil the letters under it with
    /// no pet drawn.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn pin_pet_body(&mut self, body: Option<(i32, i32, i32, i32)>) {
        self.pet_body_pinned = body;
    }

    /// The pet's body box the frame's offer noted, window pixels
    /// ([`Self::note_pet_body`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn pet_body_box(&self) -> Option<(i32, i32, i32, i32)> {
        self.pet_body
    }

    /// The pet's body box the sky last drew no star on, window pixels
    /// (`None`: off glass, or no frame offered one).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn pet_shade(&self) -> Option<(i32, i32, i32, i32)> {
        self.v2.pet_shade()
    }

    /// The boxes the sky and the ribbon last kept off, window pixels
    /// ([`Self::keep_out_boxes`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn keep_out(&self) -> &[(i32, i32, i32, i32)] {
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

    /// The word icons on glass ([`Self::note_word_icons`]), window pixels
    /// `(x0, x1, y0, y1)`: `trail status`'s `word_icons=`, which the trail
    /// gallery keeps per frame for `lab trail-companion`.
    #[inline]
    pub fn word_icon_window_boxes(&self) -> impl Iterator<Item = (i32, i32, i32, i32)> + '_ {
        let (ox, oy) = self.keep_geom.map_or((0, 0), |(ox, oy, _)| (ox, oy));
        self.word_icons
            .iter()
            .map(move |&(x0, x1, y0, y1)| (x0 + ox, x1 + ox, y0 + oy, y1 + oy))
    }

    #[inline]
    pub(crate) fn clear_word_icons(&mut self) {
        self.word_icons.clear();
        self.word_icon_src.clear();
        self.word_icons_unshaded = true;
    }

    /// This frame's [`rk::companion::KeepOut`] for the ribbon and its sky,
    /// window pixels: the word icons, and each run of lettered or filled
    /// cells (a ground that is not the page) of every row of the frame's
    /// text but the caret's.
    #[inline]
    pub(super) fn keep_out_boxes(&mut self, cfg: &GlowConfig, geom: Geom, cur: Option<(u16, u16)>) {
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        self.keep_geom = Some((ox, oy, cw));
        let mut boxes = std::mem::take(&mut self.keep_out_scratch);
        boxes.clear();
        let page = cfg.theme_bg & 0x00FF_FFFF;
        let ft = &self.frame_text;
        let mut rows = std::mem::take(&mut self.keep_rows_scratch);
        rows.clear();
        let mut filled = std::mem::take(&mut self.keep_filled_scratch);
        filled.clear();
        let mut letters = std::mem::take(&mut self.keep_letters_scratch);
        letters.clear();
        // A short glyph's box begins where its ink may ([`super::emit::SHORT_GLYPH_TOP_CH`]):
        // the in-row rail dips into its cell's blank top under a descender above it.
        let short_top = (super::emit::SHORT_GLYPH_TOP_CH * ch as f32).floor() as i32;
        #[cfg(test)]
        let short_top = if self.letters_whole_cells {
            0
        } else {
            short_top
        };
        let mut last_ground: Option<(u32, bool)> = None;
        let mut i = 0;
        while i < ft.rows.len() {
            let row = ft.lo + i as i32;
            if matches!(cur, Some((cr, _)) if i32::from(cr) == row) {
                i += 1;
                continue;
            }
            let y0 = oy + row * ch;
            let y1 = y0 + ch;
            let y0_short = y0 + short_top;
            let row_pairs = match ft.pairs.get(i * ft.cols..(i + 1) * ft.cols) {
                Some(s) => s,
                None => &[],
            };
            let mut run: Option<i32> = None;
            let mut fill_run: Option<i32> = None;
            let mut letter_run: Option<(i32, bool)> = None;
            let mut span: Option<(i32, i32)> = None;
            let mut idx = 0;
            while idx < row_pairs.len() {
                let pair = row_pairs[idx];
                let col = ft.col0 + idx as i32;
                idx += 1;
                let fill = match pair {
                    Some((_, ground)) => {
                        let g = ground & 0x00FF_FFFF;
                        if g == page {
                            false
                        } else {
                            match last_ground {
                                Some((prev_g, prev_held)) if prev_g == g => prev_held,
                                _ => {
                                    let h = !super::emit::reads_as_page(g, page);
                                    last_ground = Some((g, h));
                                    h
                                }
                            }
                        }
                    }
                    None => false,
                };
                let held = fill || matches!(pair, Some((Some(_), _)));
                match (held, run) {
                    (true, None) => run = Some(col),
                    (false, Some(c0)) => {
                        boxes.push((ox + c0 * cw, ox + col * cw, y0, y1));
                        span = Some(match span {
                            Some((a, _)) => (a, col),
                            None => (c0, col),
                        });
                        run = None;
                    }
                    _ => {}
                }
                match (fill, fill_run) {
                    (true, None) => fill_run = Some(col),
                    (false, Some(c0)) => {
                        filled.push((ox + c0 * cw, ox + col * cw, y0, y1));
                        fill_run = None;
                    }
                    _ => {}
                }
                let lettered = matches!(pair, Some((Some(_), _)));
                let short = lettered && ft.short(row, col);
                match letter_run {
                    Some((c0, was_short)) if !lettered || was_short != short => {
                        let top = if was_short { y0_short } else { y0 };
                        letters.push((ox + c0 * cw, ox + col * cw, top, y1));
                        letter_run = lettered.then_some((col, short));
                    }
                    None if lettered => letter_run = Some((col, short)),
                    _ => {}
                }
            }
            let end = ft.col0 + row_pairs.len() as i32;
            if let Some(c0) = run {
                boxes.push((ox + c0 * cw, ox + end * cw, y0, y1));
                span = Some(match span {
                    Some((a, _)) => (a, end),
                    None => (c0, end),
                });
            }
            if let Some(c0) = fill_run {
                filled.push((ox + c0 * cw, ox + end * cw, y0, y1));
            }
            if let Some((c0, was_short)) = letter_run {
                let top = if was_short { y0_short } else { y0 };
                letters.push((ox + c0 * cw, ox + end * cw, top, y1));
            }
            // For a dark train the span reaches a cell past the row's last
            // letter, where its caret stood when it was typed: a Return's
            // train roots there, and cut at the letters its root stood alone
            // after the line as a brown stub (looks6 round 3). A paper train
            // keeps to the letters (`KeepOut::past`).
            if let Some((c0, c1)) = span {
                rows.push((ox + c0 * cw, ox + c1 * cw, y0, y1));
            }
            i += 1;
        }
        let mut feet = std::mem::take(&mut self.keep_feet_scratch);
        let mut soles = [None; 2];
        self.foot_boxes(geom, &mut feet, &mut soles);
        let (mut sole_boxes, mut n) = ([(0, 0, 0, 0); 2], 0);
        let mut k = 0;
        while k < soles.len() {
            if let Some(sole) = soles[k] {
                sole_boxes[n] = sole;
                n += 1;
            }
            k += 1;
        }
        self.v2.note_keep_out_slices(
            &self.word_icons,
            (ox, oy),
            &boxes,
            (&rows, cw),
            (&filled, &letters),
            (&feet, &sole_boxes[..n]),
        );
        self.keep_letters_scratch = letters;
        self.keep_feet_scratch = feet;
        self.keep_out_scratch = boxes;
        self.keep_rows_scratch = rows;
        self.keep_filled_scratch = filled;
    }

    /// The feet of the rows whose glyphs the host named (the caret's row
    /// and the row before it, probed), window pixels: each run of cells
    /// whose ink may reach under the baseline (a descender, a wide or
    /// non-ASCII glyph, a letter the probe did not name) into `feet`, and
    /// each such row's box into `soles` ([`rk::companion::KeepOut::foot_share`]).
    #[inline]
    pub(super) fn foot_boxes(
        &self,
        geom: Geom,
        feet: &mut Vec<(i32, i32, i32, i32)>,
        soles: &mut [Option<(i32, i32, i32, i32)>; 2],
    ) {
        feet.clear();
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        let cols = geom.cols as i32;
        let probed = [
            (self.row_cur_meta, self.row_cur.as_slice()),
            (self.row_prev_meta, self.row_prev.as_slice()),
        ];
        let mut k = 0;
        while k < probed.len() {
            let (meta, chars) = probed[k];
            let cur_k = k;
            k += 1;
            let Some(m) = meta else {
                continue;
            };
            let row = i32::from(m.row);
            let y0 = oy + row * ch;
            let sole = (ox, ox + cols * cw, y0, y0 + ch);
            if soles[0] == Some(sole) || soles[1] == Some(sole) {
                continue;
            }
            soles[cur_k] = Some(sole);
            let mut run: Option<i32> = None;
            let mut col = 0;
            while col <= cols {
                // A cell is a foot where the probe names a glyph that may
                // descend, or where it names none and the frame's own cells
                // hold ink: the probe may be a frame behind the frame, and
                // read by it alone the newest `q` was a blank and a rail in
                // its row's foot band ran under its tail (looks10 L2).
                let foot = col < cols && {
                    let c = col as usize;
                    let frame_inked = matches!(self.frame_text.pair(row, col), Some((Some(_), _)));
                    match chars.get(c) {
                        Some(' ') | None => frame_inked,
                        Some(&g) => {
                            g == '\0'
                                || chars.get(c + 1) == Some(&'\0')
                                || !g.is_ascii()
                                || super::emit::is_descender_glyph(g)
                        }
                    }
                };
                match (foot, run) {
                    (true, None) => run = Some(col),
                    (false, Some(c0)) => {
                        feet.push((ox + c0 * cw, ox + col * cw, y0, y0 + ch));
                        run = None;
                    }
                    _ => {}
                }
                col += 1;
            }
        }
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
    pub const fn take_flow_entry(&mut self) -> bool {
        let open = self.flow_status().open();
        let entered = open && !self.flow_open;
        self.flow_open = open;
        entered
    }

    /// Pin typing momentum to at least `drive` during a live celebration ([`crate::kitty_sing`]).
    #[inline]
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
    #[inline]
    pub fn hush_fanfare(&mut self, now: Instant) {
        self.unsettle();
        self.momentum.set_value(now, 0.0);
        self.flow_open = false;
        if self.v2.engaged() {
            self.v2.hush_fanfare(now);
        }
    }
}
