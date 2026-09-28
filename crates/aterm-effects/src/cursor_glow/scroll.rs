// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SCROLL AND BAND MOVES — translating every row-addressed piece of state
//! when the viewport scrolls or a TUI band moves under the caret.

use super::*;

/// **THE ROW-BAND LAW** — where a grid row lands when the host reports that
/// screen rows `top..=bottom` moved by `delta` rows, for a MARK (a ribbon
/// cell, a spark, a classifier anchor, a veil): a row outside the band is
/// untouched (the band is the only thing that moved), a row inside it moves
/// by `delta`, and a row whose new index leaves the band is **GONE** — `None`
/// — never clamped to the band's edge. The same law [`CursorGlow::note_scroll`]
/// applies to the whole grid, restated for a sub-range: clamping a row that
/// scrolled out piles the whole departed trail into a bright bar on the edge
/// row, and a classifier anchor clamped there becomes a false source for the
/// next observed move (a cursor position that never existed).
///
/// The producers are the terminal's region scrolls: Codex's inline viewport
/// riding DOWN one row per streamed line (`ESC[{vt};57r … RI`: the band
/// `[vt..56] +1`), its pinned transcript archiving UP (`ESC[1;52r … LF`: the
/// band `[0..51] −1`), an Enter's `RI×k` (`[11..56] +k`), tmux/vim/less
/// status rows, IL/DL. The grid records each one with its numbers
/// (`aterm_grid::RowBandMove`), the core keeps them in `ContentScrollState`'s
/// explained-motion ring, and the host replays them oldest-first through
/// `note_band_move` on the present that observed the batch — so the next tick
/// already sees every mark where its text now is (frame-0, nothing judged).
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

/// [`band_row`] for a POSITION rather than a mark — the caret, the cell the
/// last Backspace emptied: a position cannot be dropped (the host observes it
/// again on its next move, and the engine must answer `field_at_caret` in
/// between), so a row carried past the band's edge saturates AT that edge,
/// exactly as the scroll path's `saturating_sub` parks a scrolled-out caret
/// on row 0. Outside the band the row is untouched.
#[inline]
#[must_use]
pub fn band_pos(row: u16, top: u16, bottom: u16, delta: i16) -> u16 {
    if !(top..=bottom).contains(&row) {
        return row;
    }
    let moved = (i32::from(row) + i32::from(delta)).clamp(i32::from(top), i32::from(bottom));
    moved as u16
}

/// [`band_row`] for the PIXEL-ADDRESSED pools — the starfield, the landing
/// ring, the fire meteors, vapor, bolts, and v2's stars, flights and landings
/// are window-absolute px, so the band's three row numbers are restated as
/// the half-open pixel span `[y_lo, y_hi)` = `[origin_y + top·ch, origin_y +
/// (bottom+1)·ch)` and the displacement `dy = delta·ch`. The law is the cell
/// law pixel for pixel: a point inside the span moves by `dy` and is kept iff
/// it is still inside; a point outside is untouched. A SPAN (a meteor's
/// tail→head, a bolt's polyline) moves whole when every point is inside, is
/// left alone when none is, and is DROPPED when it straddles the band edge —
/// half of it belongs to text that moved and half to text that did not, and a
/// streak bent or stretched to fit would be light over cells neither end
/// earned.
///
/// `origin_y` is the grid interior's top in window px: without it a chrome
/// band above the grid (a titlebar, a tab strip) mis-places every band edge
/// by its own height. A caller with no geometry yet (`cell_h == 0`, no tick
/// observed) must not build one of these: the span would be empty and every
/// pool would read as "outside", which is the one answer a fail-closed
/// translate may not give — it drops the pools instead, exactly as
/// `CursorGlow::translate_scroll_state` does (private, so named and not
/// linked).
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

    /// A two-point SPAN (`y0` to `y1`): both inside → both move, kept iff
    /// both still inside; neither inside → untouched and kept; one inside →
    /// dropped. The return is the `retain` verdict.
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
    /// A PTY-driven scroll shifted the whole screen up `rows` lines this
    /// frame: translate the trail anchors so the NEXT move classifies against
    /// where the previous caret cell NOW sits. Without this, a wrap that GROWS
    /// a bottom-anchored TUI box (Claude Code with a full transcript — the
    /// dominant long-session state) scrolls the screen one line in the same
    /// repaint, the caret's observed move flattens to dr==0 with a huge column
    /// delta, the re-anchor classifier (which requires dr==1) misses, and the
    /// full jump choreography fires over the line. Alt screens have no
    /// scrollback, so vim's scrolls never translate
    /// (their jumps keep the owner-mandated drama).
    pub fn note_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.recent_typed_run = None;
        self.carry_held_park(|row| row.checked_sub(rows));
        // A plain-Backspace classifier and its legacy `(row, fill)` baseline
        // describe the pre-scroll content stream. Even before the host's
        // companion `drop_row_probe` call lands, they must not survive this
        // fence and compare against unrelated text that later occupies the
        // same numeric row (a phantom erase poof/trail).
        self.bs_poof_hint = None;
        self.bs_baseline = None;
        self.quench_hint = None;
        self.translate_scroll_state(rows, self.last_ch);
    }

    /// A HELD PARK ([`HeldPark`]) RIDES A SCROLL OR A BAND MOVE WITH ITS
    /// ROW, as `last`, `last_visible`, the sparks, the v2 mirror and the
    /// buffered events ride both edges (`map`: an absolute row to its new
    /// row, `None` once carried off the top or past the band's edge).
    /// Carried off there is no honest row: it is judged first, in the
    /// pre-move space, then everything it laid translates with the rest.
    /// Flushed at the edge instead, a transcript line Claude Code printed
    /// inside Ink's ~40 ms park/rewrite gap judged the park as a typed
    /// re-anchor at its own clock (the key's stamp spent on the prompt
    /// cell) and refused the real return on the moved row `no-fresh-hint`,
    /// the key's cell dark. Called BEFORE the callers' poof / baseline /
    /// quench clears: the flush's judgment reads `quench_hint`.
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

    /// Drop the echo-anchor row memory. It names absolute rows, so it is
    /// dropped — never translated — wherever the rows it names change
    /// content or coordinate space (a scroll, a band move, a reset): a kept
    /// launch column would anchor a sweep against unrelated text.
    pub(super) fn forget_anchor_rows(&mut self) {
        self.anchor_rows = [None; ANCHOR_ROWS];
        self.anchor_rows_head = 0;
        self.last_anchor_sweep = None;
    }

    /// Move every ROW-ADDRESSED member under `map` (an absolute row to its
    /// new row, `None` once carried off the grid or past the band's edge)
    /// — the one family operation both [`Self::translate_scroll_state`] and
    /// [`Self::translate_band_state`] open with, so a member added to the
    /// family is added once. THE ROW-MAP LAW: a row outside the map's
    /// domain has no honest cell and is DROPPED, never clamped — clamping an
    /// anchor to row 0 would create a false source for the next observed
    /// move, and clamping light would pile the scrolled-away trail into a
    /// bright bar. The classifier anchors (`last`, `last_visible`, the
    /// hide-bridge estimate), the paid pending-wrap cell (a shell's
    /// bottom-row wrap scrolls before the fold is observed) and the sparks
    /// ride the map; the echo-anchor row memory names absolute rows whose
    /// content just moved and is dropped ([`Self::forget_anchor_rows`]);
    /// the insert's members follow their own law
    /// ([`Self::translate_insert_rows`]).
    pub(super) fn translate_row_addressed(&mut self, map: &impl Fn(u16) -> Option<u16>) {
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

    /// Move every live, position-bearing member with a PTY scroll.  `cell_h`
    /// comes from the OUTERMOST engine because an outgoing style ghost may not
    /// have ticked yet (and therefore may not have populated its own
    /// [`Self::last_ch`]), even though its forged pixel geometry is already on
    /// glass in this same coordinate space.
    ///
    /// This is deliberately one family operation rather than a list split
    /// between [`Self::note_scroll`] and individual emitters.  A member added to
    /// the transient-light family must either be translated here or be dropped
    /// when its geometry is unknown/off-top; leaving it untouched strands the
    /// previous frame's light over unrelated post-scroll cells.
    pub(super) fn translate_scroll_state(&mut self, rows: u16, cell_h: u16) {
        self.translate_row_addressed(&|row: u16| row.checked_sub(rows));
        // THE PIXEL-ADDRESSED POOLS. The two above are addressed by grid cell,
        // so a row subtraction moves them. The starfield, the landing ring, the
        // ZOOM streaks, landing starbursts, fast-glide stars/landing anchor,
        // fire meteors, vapor and bolt polylines are addressed in WINDOW-
        // ABSOLUTE PIXELS instead. Leaving even one family behind lets it hang
        // in place while the text slides out from under it; a saved glide head
        // is worse, because it mints a NEW landing burst later at the stale Y.
        //
        // They need a pixel translation, and `note_scroll` is handed only a row
        // count, which is why the earlier sweep could not carry them. `last_ch`
        // closes that gap. With no tick yet observed the height is unknown, so
        // every pool is DROPPED — fail closed, exactly as the cell-addressed
        // pools drop anything pushed past the top rather than clamping it into a
        // bar along row 0.
        let dy = f32::from(rows) * f32::from(cell_h);
        if cell_h == 0 {
            self.particles.clear();
            self.ring = None;
            self.fire_meteors.clear();
            self.vapor.clear();
            self.bolts.clear();
        } else {
            // Dropped once past the window top, with ONE CELL of allowance —
            // the starfield deliberately places light in the sky band just
            // above row 0, so the cut cannot sit exactly at zero. Measured
            // against the window, never against this scroll's distance: a pool
            // that survived an earlier scroll must not be rescued by a later,
            // larger one.
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

        // OUTGOING STYLE GHOSTS paint into the same frame and can outlive the
        // switch by 250 ms.  Recursing with the outer row height covers both a
        // normally ticked ghost and the one-frame seam where it owns forged
        // geometry but has not populated its own `last_ch` yet.  The frozen
        // anchor uses checked subtraction: unlike the live classifier anchor,
        // it is visible geometry, so clamping an off-top cursor to row 0 would
        // manufacture a crown there.
        for fade in &mut self.fading {
            fade.engine.translate_scroll_state(rows, cell_h);
            fade.anchor = fade
                .anchor
                .and_then(|(row, col)| row.checked_sub(rows).map(|row| (row, col)));
            fade.engine.last = fade.anchor;
            fade.engine.last_ch = cell_h;
        }
        // SEAM POINT 12 (§17.2, D14): every live v2 mark — ribbon cells,
        // meteors and landings, stars — moves with the viewport too; the
        // engine drops what leaves the grid and forgets its glyph probe
        // (the host re-probes before the next deal).
        if self.v2.engaged() {
            self.v2.translate_scroll(rows, cell_h);
        }
    }

    /// **A ROW BAND MOVED** — the host replays one `aterm_grid::RowBandMove`
    /// the terminal recorded this present: screen rows `top..=bottom` are
    /// now `delta` rows from where the previous frame drew them (the
    /// [`band_row`] law). The sibling of [`Self::note_scroll`] for the
    /// motion a whole-grid translation cannot express: Codex's inline
    /// viewport sliding DOWN one row per streamed line while the rows above
    /// it stand still (`[vt..56] +1`), its pinned transcript archiving UP
    /// under a fixed composer (`[0..51] −1`), an Enter's `RI×k`, a tmux pane
    /// or a vim status row scrolling inside its DECSTBM region, IL/DL.
    /// Before this fence existed every one of those reached the engine as an
    /// epoch step → `reset()`: the measured Codex session read
    /// `ribbon_segments 16 → 0` on the FIRST streamed line and `1, 1, 1, 0,
    /// 0…` for the rest, with every banked key and the momentum thrown away
    /// per line (measured on glass 2026-09-10; that record was never
    /// committed). A band move is a COORDINATE TRANSFORM and never a licence
    /// (T1): nothing is judged, nothing spawns, and the next observed move
    /// classifies against where the previous caret cell NOW sits.
    ///
    /// What is dropped here and what is kept follows the hint's meaning, not
    /// the scroll path blindly: the plain-Backspace poof classifier and its
    /// `(row, fill)` baseline describe the PRE-move row content and are
    /// retired (as `note_scroll` retires them — the host's `drop_row_probe`
    /// lands beside this call). `quench_hint` is **KEPT**, unlike the scroll
    /// path: it is the Backspace's KEY-TIME licence, and under Codex the
    /// erase's echo routinely lands AFTER a streamed line has slid the
    /// composer — retiring it would decline the user's own Backspace as
    /// `no-fresh-hint` on exactly the frames this fence exists to survive
    /// (`a_backspace_echo_that_lands_after_a_band_move_is_still_licensed`).
    /// A HELD PARK ([`HeldPark`]) is a row-addressed member too
    /// ([`Self::carry_held_park`]): inside the band it rides `delta` with
    /// the row it was held on (its return is then recognised on the moved
    /// row, where `last` and the v2 mirror already sit); carried past the
    /// band's edge it is judged first, in the pre-move space.
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

    /// Move every live, position-bearing member with a ROW-BAND move — the
    /// one family operation beside [`Self::translate_scroll_state`], member
    /// by member, under the [`band_row`] law: outside the band untouched,
    /// inside the band moved by `delta`, pushed past either edge GONE.
    /// `cell_h` and `origin_y` come from the OUTERMOST engine for the reason
    /// the scroll twin gives (an outgoing style ghost may not have ticked
    /// yet); together they name the band's pixel span `y_lo = origin_y +
    /// top·ch`, `y_hi = origin_y + (bottom+1)·ch`, half-open, so the pixel
    /// pools answer the same question the cell pools do: is this point in
    /// the rows that moved?
    ///
    /// The pixel rule for SPANS (a fire meteor's tail→head, a bolt's
    /// polyline): every point inside → the whole span moves; no point inside
    /// → untouched; a span that STRADDLES the band edge is dropped — half of
    /// it belongs to text that moved and half to text that did not, and a
    /// streak bent or stretched to fit would be light over cells neither end
    /// earned. Nothing is minted for a dropped flight (no landing).
    pub(super) fn translate_band_state(
        &mut self,
        top: u16,
        bottom: u16,
        delta: i16,
        cell_h: u16,
        origin_y: u16,
    ) {
        self.translate_row_addressed(&|row: u16| band_row(row, top, bottom, delta));
        // THE PIXEL-ADDRESSED POOLS, under the band's pixel span. With no
        // tick yet observed the geometry is unknown, so every pool is
        // DROPPED — fail closed, as the scroll twin does.
        if cell_h == 0 {
            self.particles.clear();
            self.ring = None;
            self.fire_meteors.clear();
            self.vapor.clear();
            self.bolts.clear();
        } else {
            let px = BandPx::new(top, bottom, delta, cell_h, origin_y);
            // Points: analytic particles and vapor at their birth origin (the
            // whole trajectory moves without rebasing the clock), the ring at
            // its centre.
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

        // OUTGOING STYLE GHOSTS paint into the same frame (see the scroll
        // twin): recurse with the outer geometry, carry the frozen anchor
        // under the band law (it is visible geometry — dropped, not clamped,
        // when it leaves the band), and keep `cur == engine.last`.
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
        // SEAM POINT 12 (§17.2, D14): every live v2 mark moves with its band
        // too — the ribbon's cells and field index, the meteors and their
        // landings, the stars and veils, the caret — while the SPINE is left
        // alone: the momentum the hand earned does not restart because the
        // program printed a line.
        if self.v2.engaged() {
            self.v2.translate_band(top, bottom, delta, cell_h, origin_y);
        }
    }
}
