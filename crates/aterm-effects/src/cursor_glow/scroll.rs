// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scroll and band moves: translating every row-addressed piece of state when
//! the viewport scrolls or a TUI band moves under the caret.

use super::*;

/// The row-band law: where a mark's grid row lands when screen rows
/// `top..=bottom` moved by `delta`. Outside the band a row is untouched; inside
/// it moves by `delta`; a row whose new index leaves the band is gone (`None`),
/// never clamped (clamping piles a departed trail on the edge row and makes a
/// false source for the next move). The grid records each region scroll
/// (`aterm_grid::RowBandMove`) and the host replays them oldest-first through
/// [`CursorGlow::note_band_move`].
#[inline]
#[must_use]
pub fn band_row(row: u16, top: u16, bottom: u16, delta: i16) -> Option<u16> {
    if !(top..=bottom).contains(&row) {
        return Some(row);
    }
    let moved = i32::from(row) + i32::from(delta);
    (i32::from(top)..=i32::from(bottom))
        .contains(&moved)
        .then_some(moved as u16)
}

/// [`band_row`] for a position rather than a mark (the caret, the cell the last
/// Backspace emptied). A position cannot be dropped, so a row carried past the
/// band's edge saturates at that edge.
#[inline]
#[must_use]
pub fn band_pos(row: u16, top: u16, bottom: u16, delta: i16) -> u16 {
    if !(top..=bottom).contains(&row) {
        return row;
    }
    let moved = (i32::from(row) + i32::from(delta)).clamp(i32::from(top), i32::from(bottom));
    moved as u16
}

/// [`band_row`] for the pixel-addressed pools (starfield, landing ring, fire
/// meteors, vapor, bolts, v2's stars, flights and landings): the band is the
/// half-open span `[origin_y + top·ch, origin_y + (bottom+1)·ch)` in window px,
/// displaced by `delta·ch`. A point inside moves and is kept iff still inside.
/// A span moves whole when every point is inside, is untouched when none is,
/// and is dropped when it straddles the edge. With no geometry yet
/// (`cell_h == 0`) callers drop the pools instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandPx {
    pub(super) y_lo: f32,
    pub(super) y_hi: f32,
    pub(super) dy: f32,
}

impl BandPx {
    /// The band `top..=bottom` by `delta` rows, on a grid of `cell_h` px rows
    /// whose interior starts `origin_y` px down the window.
    #[must_use]
    pub fn new(top: u16, bottom: u16, delta: i16, cell_h: u16, origin_y: u16) -> Self {
        let ch = f32::from(cell_h);
        let oy = f32::from(origin_y);
        Self {
            y_lo: oy + f32::from(top) * ch,
            y_hi: oy + (f32::from(bottom) + 1.0) * ch,
            dy: f32::from(delta) * ch,
        }
    }

    /// Whether a window-absolute `y` lies in the rows that moved.
    #[inline]
    #[must_use]
    pub fn inside(&self, y: f32) -> bool {
        (self.y_lo..self.y_hi).contains(&y)
    }

    /// A point: moved iff inside, kept iff still inside; outside, untouched and
    /// kept. Returns the `retain` verdict.
    #[inline]
    pub fn point(&self, y: &mut f32) -> bool {
        if !self.inside(*y) {
            return true;
        }
        *y += self.dy;
        self.inside(*y)
    }

    /// A two-point span (`y0` to `y1`): both inside, both move, kept iff both
    /// still inside; neither inside, untouched and kept; one inside, dropped.
    /// Returns the `retain` verdict.
    #[inline]
    pub fn span(&self, y0: &mut f32, y1: &mut f32) -> bool {
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
    /// A PTY-driven scroll shifted the whole screen up `rows` lines: translate
    /// the trail anchors so the next move classifies against where the previous
    /// caret cell now sits. Otherwise a wrap that grows a bottom-anchored TUI box
    /// reads as a same-row move with a huge column delta and fires the full jump.
    pub fn note_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.recent_typed_run = None;
        self.carry_held_park(|row| row.checked_sub(rows));
        // The plain-Backspace classifier and its `(row, fill)` baseline describe
        // the pre-scroll content, so they must not survive to compare against
        // unrelated text now on the same row (a phantom erase poof).
        self.bs_poof_hint = None;
        self.bs_baseline = None;
        self.quench_hint = None;
        self.translate_scroll_state(rows, self.last_ch);
        self.scroll_rows = self.scroll_rows.saturating_add(rows);
    }

    /// Carry a held park ([`HeldPark`]) with its row (`map`: an absolute row to
    /// its new row, `None` once carried off), so a line printed inside Ink's
    /// ~40 ms park/rewrite gap does not strand the key's cell. Carried off, it is
    /// judged first in the pre-move space. Runs before the callers clear
    /// `quench_hint`, which the flush reads.
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

    /// Drop the echo-anchor row memory. It names absolute rows, so it is dropped,
    /// not translated, whenever those rows change content or coordinate space (a
    /// scroll, a band move, a reset).
    pub(super) fn forget_anchor_rows(&mut self) {
        self.anchor_rows = [None; ANCHOR_ROWS];
        self.last_anchor_sweep = None;
    }

    /// Move every row-addressed member under `map` (an absolute row to its new
    /// row, `None` once carried off the grid or past the band's edge): the shared
    /// opening of [`Self::translate_scroll_state`] and
    /// [`Self::translate_band_state`]. A row outside the map's domain is dropped,
    /// never clamped. The classifier anchors (`last`, `last_visible`, the
    /// hide-bridge estimate), the paid pending-wrap cell and the sparks ride the
    /// map; the echo-anchor row memory is dropped ([`Self::forget_anchor_rows`]);
    /// the insert's members follow [`Self::translate_insert_rows`]; the hand's
    /// row ([`Self::hand_row`]) rides the map and keeps the caret's screen row
    /// beside it when the hand typed at the caret ([`HandRow::carried`]). The
    /// classic engine's sparks, ring and last cell ride the same map
    /// ([`crate::classic_wake::ClassicWake::translate_rows`]).
    pub(super) fn translate_row_addressed(&mut self, map: &impl Fn(u16) -> Option<u16>) {
        // Read before `last` moves: whether the hand was typing at the caret
        // the last frame drew.
        let hand_at_caret = self.hand_types_at_the_caret(self.last);
        self.last = self
            .last
            .and_then(|(row, col)| map(row).map(|row| (row, col)));
        self.last_visible = self
            .last_visible
            .and_then(|((row, col), at)| map(row).map(|row| ((row, col), at)));
        self.hide_bridge_shown = self
            .hide_bridge_shown
            .and_then(|(row, col)| map(row).map(|row| (row, col)));
        self.v2_stop = self.v2_stop.and_then(|(t, at)| match at {
            Some((row, col)) => map(row).map(|row| (t, Some((row, col)))),
            None => Some((t, None)),
        });
        self.forget_anchor_rows();
        // The hand's row is where the hand is: the scroll moved the input line
        // and the hand with it, unless the line was redrawn on its own screen
        // row (a bottom-anchored composer under a transcript line), which the
        // caret's next sighting settles.
        self.hand_row = self
            .hand_row
            .and_then(|hand| hand.carried(map, hand_at_caret));
        self.unowned_caret = self.unowned_caret.and_then(|mut caret| {
            caret.row = map(caret.row)?;
            Some(caret)
        });
        // A typed lay awaiting the caret's rest ([`TornLay`]) stands: the
        // content moved, so where the caret rests no longer judges it.
        self.torn_lay = None;
        self.wrap_paid_row = self.wrap_paid_row.and_then(map);
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

    /// Move every live, position-bearing member with a PTY scroll; a transient
    /// whose geometry is unknown or off-top is dropped, never left over
    /// unrelated cells. `cell_h` comes from the outermost engine: an outgoing
    /// style ghost may not have ticked (and set its own [`Self::last_ch`]).
    pub(super) fn translate_scroll_state(&mut self, rows: u16, cell_h: u16) {
        self.translate_row_addressed(&|row: u16| row.checked_sub(rows));
        // Pixel-addressed pools are window-absolute px and need a pixel
        // translation (a stale glide head would later mint a landing burst at
        // the old Y). With no tick observed the height is unknown, so every pool
        // is dropped (fail closed).
        let dy = f32::from(rows) * f32::from(cell_h);
        if cell_h == 0 {
            self.particles.clear();
            self.ring = None;
            self.fire_meteors.clear();
            self.vapor.clear();
            self.bolts.clear();
        } else {
            // Dropped once past the window top, with one cell of allowance (the
            // starfield uses the sky band just above row 0).
            let top = -f32::from(cell_h);
            // Analytic particles/vapor are translated at their birth origin;
            // that moves their whole trajectory without rebasing the clock.
            self.particles.retain_mut(|p| {
                p.y0 -= dy;
                p.y0 > top
            });
            self.ring = self.ring.take().and_then(|mut r| {
                r.cy -= dy;
                (r.cy > top).then_some(r)
            });
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
                for (_, y) in &mut bolt.pts {
                    *y -= dy;
                }
                bolt.pts.iter().any(|&(_, y)| y > top)
            });
        }

        // Outgoing style ghosts paint into the same frame for up to 250 ms:
        // recurse with the outer row height (a ghost may not have set its own
        // `last_ch`). The frozen anchor is visible geometry, so an off-top
        // cursor is dropped, not clamped to row 0.
        for fade in &mut self.fading {
            fade.engine.translate_scroll_state(rows, cell_h);
            fade.anchor = fade
                .anchor
                .and_then(|(row, col)| row.checked_sub(rows).map(|row| (row, col)));
            fade.engine.last = fade.anchor;
            fade.engine.last_ch = cell_h;
        }
        // Every live v2 mark (ribbon cells, meteors and landings, stars) moves
        // with the viewport; the engine drops what leaves the grid and forgets
        // its glyph probe (the host re-probes before the next deal).
        if self.v2.engaged() {
            self.v2.translate_scroll(rows, cell_h);
        }
    }

    /// A row band moved: the host replays one `aterm_grid::RowBandMove` recorded
    /// this present (screen rows `top..=bottom` are now `delta` rows from where
    /// the previous frame drew them; the [`band_row`] law). A coordinate
    /// transform, never a licence: nothing is judged or spawned, so banked keys
    /// and momentum survive.
    ///
    /// Unlike [`Self::note_scroll`], `quench_hint` is kept: under Codex a
    /// Backspace's echo often lands after a streamed line slid the composer.
    /// Each replay is counted ([`Self::band_moves`]).
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

    /// Whether input still owes an echo: a typed stamp, an unpaid typed cell,
    /// or an erase whose retreat is still owed. This sustains an existing
    /// hand through a content-proved row move; it grants no movement licence.
    pub(super) fn key_in_flight(&self, now: Instant) -> bool {
        self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH)
            || self.type_press_ring.any_within(now)
            || self.last_erase.is_some_and(|erase| erase.owed_at(now))
    }

    /// The seam's half of the hand riding its line ([`rk::Engine::lift_hand`]):
    /// the follow pass proved the hand's own line repainted on row `to`
    /// (a composer's box grown or shrunk a row with no scroll), so the
    /// hand-addressed members on row `from` ride with it as a band move would
    /// carry them: the last observed caret, its visible twin and the hide-bridge
    /// estimate, the hand's row, the recent typed run, the torn lay and the
    /// cell it saw, a held park, the last licensed and anchor-swept rows, the
    /// paid wrap cell and the insert seam's rows
    /// ([`Self::translate_insert_rows`]). The next caret move is then judged in
    /// the text's own frame, a key's same-row echo. The hand's line keeps its
    /// echo-anchor memory (its end and its program-row brand) on its new row,
    /// whose old entry described text that is gone; every other row keeps its
    /// own.
    pub(super) fn lift_hand_row(&mut self, from: u16, to: u16) {
        let map = |r: u16| if r == from { to } else { r };
        self.last = self.last.map(|(r, c)| (map(r), c));
        self.last_visible = self.last_visible.map(|((r, c), at)| ((map(r), c), at));
        self.hide_bridge_shown = self.hide_bridge_shown.map(|(r, c)| (map(r), c));
        if let Some((_, Some((row, _)))) = &mut self.v2_stop {
            *row = map(*row);
        }
        // The retained source probes describe that same line. Their neighbour
        // captures have no such identity proof and cannot ride with it.
        for probe in self
            .row_prev_meta
            .iter_mut()
            .chain(self.row_shown_meta.iter_mut())
            .filter(|probe| probe.row == from)
        {
            probe.row = to;
            probe.above = NbrProbe::Unprobed;
            probe.below = NbrProbe::Unprobed;
        }
        self.hand_row = self.hand_row.map(|hand| {
            if hand.row == from {
                HandRow::at(to)
            } else {
                hand
            }
        });
        if let Some(caret) = &mut self.unowned_caret {
            caret.row = map(caret.row);
        }
        if let Some(run) = &mut self.recent_typed_run {
            run.row = map(run.row);
        }
        if let Some(lay) = &mut self.torn_lay {
            lay.row = map(lay.row);
            lay.seen = lay.seen.map(|(r, c)| (map(r), c));
        }
        self.carry_held_park(|r| Some(map(r)));
        self.last_licensed_row = self.last_licensed_row.map(|(r, at)| (map(r), at));
        self.last_anchor_sweep = self.last_anchor_sweep.map(|(r, at)| (map(r), at));
        self.wrap_paid_row = self.wrap_paid_row.map(map);
        self.insert.translate_rows(|r| Some(map(r)));
        for slot in &mut self.anchor_rows {
            match slot {
                Some(m) if m.row == to => *slot = None,
                Some(m) if m.row == from => m.row = to,
                _ => {}
            }
        }
    }

    /// Move every live, position-bearing member with a row-band move, under the
    /// [`band_row`] law. `cell_h` and `origin_y` come from the outermost engine
    /// (see the scroll twin) and give the pixel pools the band's span
    /// ([`BandPx`]). A dropped flight mints no landing.
    pub(super) fn translate_band_state(
        &mut self,
        top: u16,
        bottom: u16,
        delta: i16,
        cell_h: u16,
        origin_y: u16,
    ) {
        self.translate_row_addressed(&|row: u16| band_row(row, top, bottom, delta));
        // Pixel-addressed pools under the band's pixel span. With no tick
        // observed the geometry is unknown, so every pool is dropped.
        if cell_h == 0 {
            self.particles.clear();
            self.ring = None;
            self.fire_meteors.clear();
            self.vapor.clear();
            self.bolts.clear();
        } else {
            let px = BandPx::new(top, bottom, delta, cell_h, origin_y);
            // Points: analytic particles and vapor at their birth origin (the
            // trajectory moves without rebasing the clock), the ring at its
            // centre.
            self.particles.retain_mut(|p| px.point(&mut p.y0));
            if let Some(mut r) = self.ring.take() {
                self.ring = px.point(&mut r.cy).then_some(r);
            }
            self.vapor.retain_mut(|puff| px.point(&mut puff.y0));
            // Spans: a fire meteor's tail→head, a bolt's polyline.
            self.fire_meteors
                .retain_mut(|meteor| px.span(&mut meteor.y0, &mut meteor.y1));
            self.bolts.retain_mut(|bolt| {
                let n = bolt.pts.iter().filter(|&&(_, y)| px.inside(y)).count();
                if n == 0 {
                    return true;
                }
                if n != bolt.pts.len() {
                    return false;
                }
                let mut keep = true;
                for (_, y) in &mut bolt.pts {
                    *y += px.dy;
                    keep &= px.inside(*y);
                }
                keep
            });
        }

        // Outgoing style ghosts: recurse with the outer geometry, carry the
        // frozen anchor under the band law (dropped, not clamped, when it leaves
        // the band), and keep `cur == engine.last`.
        for fade in &mut self.fading {
            fade.engine
                .translate_band_state(top, bottom, delta, cell_h, origin_y);
            fade.anchor = fade
                .anchor
                .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
            fade.engine.last = fade.anchor;
            fade.engine.last_ch = cell_h;
            fade.engine.last_origin_y = origin_y;
        }
        // Every live v2 mark moves with its band (ribbon cells and field index,
        // meteors and landings, stars and veils, the caret); the spine is left
        // alone, so momentum does not restart when a program prints a line.
        if self.v2.engaged() {
            self.v2.translate_band(top, bottom, delta, cell_h, origin_y);
        }
    }
}
