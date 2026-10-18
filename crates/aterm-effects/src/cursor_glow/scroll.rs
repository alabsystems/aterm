// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scroll and band moves: translating every row-addressed piece of state when
//! the viewport scrolls or a TUI band moves under the caret.

use super::*;

/// The row-band law: where a grid row lands when screen rows `top..=bottom`
/// moved by `delta`, for a mark (a ribbon cell, a spark, a classifier anchor, a
/// veil). A row outside the band is untouched, a row inside moves by `delta`,
/// and a row whose new index leaves the band is gone (`None`), never clamped:
/// clamping would pile a departed trail into a bar on the edge row, or make a
/// false source for the next observed move. [`CursorGlow::note_scroll`] applies
/// the same law to the whole grid.
///
/// Producers are the terminal's region scrolls: Codex's inline viewport riding
/// down per streamed line (`ESC[{vt};57r … RI`: `[vt..56] +1`), its transcript
/// archiving up (`ESC[1;52r … LF`: `[0..51] −1`), an Enter's `RI×k`, tmux/vim/
/// less status rows, IL/DL. The grid records each (`aterm_grid::RowBandMove`),
/// the core keeps them in `ContentScrollState`'s explained-motion ring, and the
/// host replays them oldest-first through `note_band_move` on the present that
/// observed the batch, so the next tick sees every mark where its text now is.
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
/// Backspace emptied). A position cannot be dropped (the engine must still
/// answer `field_at_caret`), so a row carried past the band's edge saturates at
/// that edge, as the scroll path parks a scrolled-out caret on row 0. Outside
/// the band the row is untouched.
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
/// meteors, vapor, bolts, v2's stars, flights and landings): the band becomes
/// the half-open pixel span `[y_lo, y_hi)` = `[origin_y + top·ch, origin_y +
/// (bottom+1)·ch)` with displacement `dy = delta·ch`. A point inside moves by
/// `dy` and is kept iff still inside; a point outside is untouched. A span (a
/// meteor's tail→head, a bolt's polyline) moves whole when every point is
/// inside, is untouched when none is, and is dropped when it straddles the edge.
///
/// `origin_y` is the grid interior's top in window px (chrome above the grid
/// would otherwise shift every band edge). A caller with no geometry yet
/// (`cell_h == 0`) must not build one: every pool would read as outside, so it
/// drops the pools instead, as `CursorGlow::translate_scroll_state` does.
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

    /// A POINT: moved iff inside, kept iff still inside; outside → untouched
    /// and kept. The return is the `retain` verdict.
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
    /// A PTY-driven scroll shifted the whole screen up `rows` lines this frame:
    /// translate the trail anchors so the next move classifies against where the
    /// previous caret cell now sits. A wrap that grows a bottom-anchored TUI box
    /// (Claude Code with a full transcript) scrolls in the same repaint; without
    /// this the caret's move flattens to dr==0 with a huge column delta and the
    /// full jump choreography fires. Alt screens have no scrollback, so vim's
    /// scrolls never translate.
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
    }

    /// A held park ([`HeldPark`]) rides a scroll or band move with its row, as
    /// `last`, `last_visible`, the sparks, the v2 mirror and the buffered events
    /// do (`map`: an absolute row to its new row, `None` once carried off the top
    /// or past the band's edge). Carried off, it has no honest row, so it is
    /// judged first in the pre-move space; then everything it laid translates.
    /// A transcript line printed inside Ink's ~40 ms park/rewrite gap therefore
    /// does not strand the key's cell. Called before the callers' poof,
    /// baseline and quench clears: the flush reads `quench_hint`.
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
    /// beside it when the hand typed at the caret ([`HandRow::carried`]).
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
        self.forget_anchor_rows();
        // The hand's row is where the hand is: the scroll moved the input line
        // and the hand with it, unless the line was redrawn on its own screen
        // row (a bottom-anchored composer under a transcript line), which the
        // caret's next sighting settles.
        self.hand_row = self
            .hand_row
            .and_then(|hand| hand.carried(map, hand_at_caret));
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
    }

    /// Move every live, position-bearing member with a PTY scroll. `cell_h`
    /// comes from the outermost engine because an outgoing style ghost may not
    /// have ticked (and set its own [`Self::last_ch`]) though its geometry is
    /// already on glass.
    ///
    /// One family operation: a member of the transient-light family is either
    /// translated here or dropped when its geometry is unknown or off-top, never
    /// left over unrelated post-scroll cells.
    pub(super) fn translate_scroll_state(&mut self, rows: u16, cell_h: u16) {
        self.translate_row_addressed(&|row: u16| row.checked_sub(rows));
        // Pixel-addressed pools: the starfield, landing ring, zoom streaks,
        // landing starbursts, fast-glide stars and landing anchor, fire meteors,
        // vapor and bolt polylines are window-absolute px and need a pixel
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
            // starfield uses the sky band just above row 0). Measured against
            // the window, not this scroll's distance.
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

        // Outgoing style ghosts paint into the same frame and can outlive the
        // switch by 250 ms; recurse with the outer row height, which covers a
        // ghost that has not yet set its own `last_ch`. The frozen anchor uses
        // checked subtraction: it is visible geometry, so an off-top cursor is
        // not clamped to row 0.
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
    /// the previous frame drew them; the [`band_row`] law). The sibling of
    /// [`Self::note_scroll`] for motion a whole-grid translation cannot express:
    /// Codex's inline viewport sliding down per streamed line (`[vt..56] +1`),
    /// its transcript archiving up under a fixed composer (`[0..51] −1`), an
    /// Enter's `RI×k`, a tmux pane or vim status row scrolling in its DECSTBM
    /// region, IL/DL. A band move is a coordinate transform, never a licence:
    /// nothing is judged or spawned, and the next move classifies against where
    /// the previous caret cell now sits, so banked keys and momentum survive.
    ///
    /// The plain-Backspace poof classifier and its `(row, fill)` baseline
    /// describe pre-move content and are retired (the host's `drop_row_probe`
    /// lands beside this). `quench_hint` is kept, unlike the scroll path: under
    /// Codex a Backspace's echo often lands after a streamed line slid the
    /// composer
    /// (`a_backspace_echo_that_lands_after_a_band_move_is_still_licensed`). A
    /// held park ([`HeldPark`]) rides `delta` with its row inside the band;
    /// carried past the edge it is judged first ([`Self::carry_held_park`]).
    /// `coord_band_moves` counts the replay for `trail status`.
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

    /// Whether a typed key is in flight: a fresh typed stamp, or a press whose
    /// cell no echo has laid yet. The hand rides its line only under a key
    /// ([`Self::lift_hand_row`]).
    pub(super) fn key_in_flight(&self, now: Instant) -> bool {
        self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH)
            || self.type_press_ring.presses_within(now) > 0
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
        self.translate_insert_rows(&|r| Some(map(r)));
        for slot in &mut self.anchor_rows {
            match slot {
                Some(m) if m.row == to => *slot = None,
                Some(m) if m.row == from => m.row = to,
                _ => {}
            }
        }
    }

    /// Move every live, position-bearing member with a row-band move, under the
    /// [`band_row`] law: outside untouched, inside moved by `delta`, past either
    /// edge gone. `cell_h` and `origin_y` come from the outermost engine (see
    /// the scroll twin) and name the band's half-open pixel span `y_lo =
    /// origin_y + top·ch`, `y_hi = origin_y + (bottom+1)·ch` for the pixel
    /// pools.
    ///
    /// Spans (a fire meteor's tail→head, a bolt's polyline): all points inside,
    /// the whole span moves; none inside, untouched; straddling the edge,
    /// dropped. A dropped flight mints no landing.
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
                for (_, y) in &mut bolt.pts {
                    *y += px.dy;
                }
                bolt.pts.iter().all(|&(_, y)| px.inside(y))
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
