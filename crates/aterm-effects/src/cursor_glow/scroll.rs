// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Viewport scroll and TUI row-band translation for row- and pixel-addressed cursor effects.

use super::*;

/// Translate mark row `row` when screen rows `top..=bottom` move by `delta` (`None` if scrolled out of the band).
#[inline]
#[must_use]
pub const fn band_row(row: u16, top: u16, bottom: u16, delta: i16) -> Option<u16> {
    if row < top || row > bottom {
        return Some(row);
    }
    let moved = row as i32 + delta as i32;
    if moved >= top as i32 && moved <= bottom as i32 {
        Some(moved as u16)
    } else {
        None
    }
}

/// Translate position row `row` when screen rows `top..=bottom` move by `delta`, clamping inside the band.
#[inline]
#[must_use]
pub const fn band_pos(row: u16, top: u16, bottom: u16, delta: i16) -> u16 {
    if row < top || row > bottom {
        return row;
    }
    let moved = row as i32 + delta as i32;
    if moved < top as i32 {
        top
    } else if moved > bottom as i32 {
        bottom
    } else {
        moved as u16
    }
}

/// Pixel-space row-band translator for particle and trail pools.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandPx {
    pub(super) y_lo: f32,
    pub(super) y_hi: f32,
    pub(super) dy: f32,
}

impl BandPx {
    /// Construct a pixel-space band translator for rows `top..=bottom` shifted by `delta` on `cell_h`-pixel rows.
    #[inline]
    #[must_use]
    pub const fn new(top: u16, bottom: u16, delta: i16, cell_h: u16, origin_y: u16) -> Self {
        let ch = cell_h as f32;
        let oy = origin_y as f32;
        Self {
            y_lo: oy + top as f32 * ch,
            y_hi: oy + (bottom as f32 + 1.0) * ch,
            dy: delta as f32 * ch,
        }
    }

    /// Whether window-absolute `y` falls within the moved row band.
    #[inline]
    #[must_use]
    pub const fn inside(&self, y: f32) -> bool {
        y >= self.y_lo && y < self.y_hi
    }

    /// Translate `y` if inside the band and return whether the point remains valid.
    #[inline]
    pub const fn point(&self, y: &mut f32) -> bool {
        if !self.inside(*y) {
            return true;
        }
        *y += self.dy;
        self.inside(*y)
    }

    /// Translate two-point span `(y0, y1)` and return whether the span remains intact.
    #[inline]
    pub const fn span(&self, y0: &mut f32, y1: &mut f32) -> bool {
        match (self.inside(*y0), self.inside(*y1)) {
            (false, false) => true,
            (true, true) => {
                *y0 += self.dy;
                *y1 += self.dy;
                self.inside(*y0) && self.inside(*y1)
            }
            _ => false,
        }
    }
}

impl CursorGlow {
    /// Translate row- and pixel-addressed state after a full-screen upward PTY scroll of `rows` lines.
    #[inline]
    pub fn note_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.recent_typed_run = None;
        self.carry_held_park(|row| row.checked_sub(rows));
        self.bs_poof_hint = None;
        self.bs_baseline = None;
        self.quench_hint = None;
        self.translate_scroll_state(rows, self.last_ch);
        self.scroll_rows = self.scroll_rows.saturating_add(rows);
    }

    /// Translate a held park row via `map`, or flush it if `map` drops the row.
    #[inline]
    pub(super) fn carry_held_park(&mut self, map: impl Fn(u16) -> Option<u16>) {
        if let Some(p) = self.held_park {
            match (map(p.row), map(p.landing_row)) {
                (Some(row), Some(landing_row)) => {
                    self.held_park = Some(HeldPark {
                        row,
                        landing_row,
                        ..p
                    });
                }
                _ => self.flush_held_park(),
            }
        }
    }

    /// Clear echo-anchor row memory.
    #[inline]
    pub(super) const fn forget_anchor_rows(&mut self) {
        self.anchor_rows = [None; ANCHOR_ROWS];
        self.last_anchor_sweep = None;
    }

    /// Translate all row-addressed cursor-glow state through `map`.
    #[inline]
    pub(super) fn translate_row_addressed(&mut self, map: &impl Fn(u16) -> Option<u16>) {
        let hand_at_caret = self.hand_types_at_the_caret(self.last);
        if let Some((row, _)) = &mut self.last {
            match map(*row) {
                Some(r) => *row = r,
                None => self.last = None,
            }
        }
        if let Some(((row, _), _)) = &mut self.last_visible {
            match map(*row) {
                Some(r) => *row = r,
                None => self.last_visible = None,
            }
        }
        if let Some((row, _)) = &mut self.hide_bridge_shown {
            match map(*row) {
                Some(r) => *row = r,
                None => self.hide_bridge_shown = None,
            }
        }
        if let Some((_, Some((row, _)))) = &mut self.v2_stop {
            match map(*row) {
                Some(r) => *row = r,
                None => self.v2_stop = None,
            }
        }
        self.forget_anchor_rows();
        self.hand_row = match self.hand_row {
            Some(hand) => hand.carried(map, hand_at_caret),
            None => None,
        };
        if let Some(caret) = &mut self.unowned_caret {
            match map(caret.row) {
                Some(r) => caret.row = r,
                None => self.unowned_caret = None,
            }
        }
        self.torn_lay = None;
        if let Some(row) = &mut self.wrap_paid_row {
            match map(*row) {
                Some(r) => *row = r,
                None => self.wrap_paid_row = None,
            }
        }
        self.translate_insert_rows(map);
        self.sparks.retain_mut(|s| match map(s.row) {
            Some(r) => {
                s.row = r;
                true
            }
            None => false,
        });
        self.classic.translate_rows(map);
    }

    /// Translate all pixel-addressed cursor-glow pools for an upward scroll of `rows` lines.
    #[inline]
    pub(super) fn translate_scroll_state(&mut self, rows: u16, cell_h: u16) {
        self.translate_row_addressed(&|row: u16| row.checked_sub(rows));
        let dy = f32::from(rows) * f32::from(cell_h);
        if cell_h == 0 {
            self.particles.clear();
            self.ring = None;
            self.fire_meteors.clear();
            self.vapor.clear();
            self.bolts.clear();
        } else {
            let top = -f32::from(cell_h);
            self.particles.retain_mut(|p| {
                p.y0 -= dy;
                p.y0 > top
            });
            if let Some(r) = &mut self.ring {
                r.cy -= dy;
                if r.cy <= top {
                    self.ring = None;
                }
            }
            self.fire_meteors.retain_mut(|meteor| {
                meteor.y0 -= dy;
                meteor.y1 -= dy;
                meteor.y0.max(meteor.y1) > top
            });
            self.vapor.retain_mut(|puff| {
                puff.y0 -= dy;
                puff.y0 > top
            });
            self.bolts.retain_mut(|bolt| {
                let mut any_above = false;
                let pts = bolt.pts.as_mut_slice();
                let mut pi = 0;
                while pi < pts.len() {
                    let y = &mut pts[pi].1;
                    *y -= dy;
                    any_above |= *y > top;
                    pi += 1;
                }
                any_above
            });
        }

        let mut fi = 0;
        while fi < self.fading.len() {
            let fade = &mut self.fading[fi];
            fade.engine.translate_scroll_state(rows, cell_h);
            if let Some((row, _)) = &mut fade.anchor {
                match row.checked_sub(rows) {
                    Some(r) => *row = r,
                    None => fade.anchor = None,
                }
            }
            fade.engine.last = fade.anchor;
            fade.engine.last_ch = cell_h;
            fi += 1;
        }
        if self.v2.engaged() {
            self.v2.translate_scroll(rows, cell_h);
        }
    }

    /// Translate row- and pixel-addressed state for a region scroll of `top..=bottom` by `delta` rows.
    #[inline]
    pub fn note_band_move(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.recent_typed_run = None;
        self.carry_held_park(|row| band_row(row, top, bottom, delta));
        self.bs_poof_hint = None;
        self.bs_baseline = None;
        self.coord_band_moves += 1;
        self.translate_band_state(top, bottom, delta, self.last_ch, self.last_origin_y);
    }

    /// Whether a recent key press or erase still awaits its terminal echo.
    #[inline]
    pub(super) fn key_in_flight(&self, now: Instant) -> bool {
        self.type_hint.any_fresh(now, TYPE_HINT_FRESH_DUR)
            || self.type_press_ring.any_within(now)
            || match &self.last_erase {
                Some(erase) => erase.owed_at(now),
                None => false,
            }
    }

    /// Lift row- and pixel-addressed state on the active typing line from `from` to `to`.
    #[inline]
    pub(super) fn lift_hand_row(&mut self, from: u16, to: u16) {
        #[inline]
        const fn map_hand_row(r: u16, from: u16, to: u16) -> u16 {
            if r == from { to } else { r }
        }
        if let Some((r, _)) = &mut self.last
            && *r == from
        {
            *r = to;
        }
        if let Some(((r, _), _)) = &mut self.last_visible
            && *r == from
        {
            *r = to;
        }
        if let Some((r, _)) = &mut self.hide_bridge_shown
            && *r == from
        {
            *r = to;
        }
        if let Some((_, Some((row, _)))) = &mut self.v2_stop
            && *row == from
        {
            *row = to;
        }
        if let Some(probe) = self.row_prev_meta.as_mut()
            && probe.row == from
        {
            probe.row = to;
            probe.above = NbrProbe::Unprobed;
            probe.below = NbrProbe::Unprobed;
        }
        if let Some(probe) = self.row_shown_meta.as_mut()
            && probe.row == from
        {
            probe.row = to;
            probe.above = NbrProbe::Unprobed;
            probe.below = NbrProbe::Unprobed;
        }
        self.refresh_probed_text_spans();
        if let Some(hand) = &mut self.hand_row
            && hand.row == from
        {
            *hand = HandRow::at(to);
        }
        if let Some(caret) = &mut self.unowned_caret
            && caret.row == from
        {
            caret.row = to;
        }
        if let Some(run) = &mut self.recent_typed_run
            && run.row == from
        {
            run.row = to;
        }
        if let Some(lay) = &mut self.torn_lay {
            if lay.row == from {
                lay.row = to;
            }
            if let Some((r, _)) = &mut lay.seen
                && *r == from
            {
                *r = to;
            }
        }
        self.carry_held_park(|r| Some(map_hand_row(r, from, to)));
        if let Some((r, _)) = &mut self.last_licensed_row
            && *r == from
        {
            *r = to;
        }
        if let Some((r, _)) = &mut self.last_anchor_sweep
            && *r == from
        {
            *r = to;
        }
        if let Some(r) = &mut self.wrap_paid_row
            && *r == from
        {
            *r = to;
        }
        self.insert
            .translate_rows(|r| Some(map_hand_row(r, from, to)));
        let mut ai = 0;
        while ai < self.anchor_rows.len() {
            match &mut self.anchor_rows[ai] {
                Some(m) if m.row == to => self.anchor_rows[ai] = None,
                Some(m) if m.row == from => m.row = to,
                _ => {}
            }
            ai += 1;
        }
    }

    /// Translate all row- and pixel-addressed state for a row-band move.
    #[inline]
    pub(super) fn translate_band_state(
        &mut self,
        top: u16,
        bottom: u16,
        delta: i16,
        cell_h: u16,
        origin_y: u16,
    ) {
        self.translate_row_addressed(&|row: u16| band_row(row, top, bottom, delta));
        if cell_h == 0 {
            self.particles.clear();
            self.ring = None;
            self.fire_meteors.clear();
            self.vapor.clear();
            self.bolts.clear();
        } else {
            let px = BandPx::new(top, bottom, delta, cell_h, origin_y);
            self.particles.retain_mut(|p| px.point(&mut p.y0));
            if let Some(r) = &mut self.ring
                && !px.point(&mut r.cy)
            {
                self.ring = None;
            }
            self.vapor.retain_mut(|puff| px.point(&mut puff.y0));
            self.fire_meteors
                .retain_mut(|meteor| px.span(&mut meteor.y0, &mut meteor.y1));
            self.bolts.retain_mut(|bolt| {
                let pts = bolt.pts.as_mut_slice();
                if pts.is_empty() {
                    return true;
                }
                let first_in = px.inside(pts[0].1);
                let mut pi = 1;
                while pi < pts.len() {
                    if px.inside(pts[pi].1) != first_in {
                        return false;
                    }
                    pi += 1;
                }
                if !first_in {
                    return true;
                }
                let mut keep = true;
                let mut pi = 0;
                while pi < pts.len() {
                    let y = &mut pts[pi].1;
                    *y += px.dy;
                    keep &= px.inside(*y);
                    pi += 1;
                }
                keep
            });
        }

        let mut fi = 0;
        while fi < self.fading.len() {
            let fade = &mut self.fading[fi];
            fade.engine
                .translate_band_state(top, bottom, delta, cell_h, origin_y);
            if let Some((row, _)) = &mut fade.anchor {
                match band_row(*row, top, bottom, delta) {
                    Some(r) => *row = r,
                    None => fade.anchor = None,
                }
            }
            fade.engine.last = fade.anchor;
            fade.engine.last_ch = cell_h;
            fade.engine.last_origin_y = origin_y;
            fi += 1;
        }
        if self.v2.engaged() {
            self.v2.translate_band(top, bottom, delta, cell_h, origin_y);
        }
    }
}
