// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seam: judging each observed caret move.

use super::*;

/// Which lane asks [`CursorGlow::spawn`] to judge a move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SpawnLane {
    Visible,
    Anchored { insert_ok: bool },
}

/// How a held park was released by the move that reached the seam ([`CursorGlow::release_held_park`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ParkRelease {
    Return { pr: u16, pc: u16 },
    Cancelled,
}

/// What [`CursorGlow::spawn_judged`] judges: a live move, or a held park flushed at its own clock ([`CursorGlow::flush_park`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SpawnCall {
    Live,
    FlushedPark {
        landing_glyph: bool,
        landing_cell: Option<char>,
        content_followed: bool,
    },
}

impl CursorGlow {
    /// Expire the seam's holds at the tick, before this tick's move.
    pub(super) fn expire_seam_holds(&mut self, now: Instant) {
        if self
            .held_park
            .is_some_and(|p| !p.fresh(now) || (p.cross_row && self.typed_credits_within(now) == 0))
        {
            self.flush_held_park();
        }
        if let Some(orphans) = self.insert.retire_orphans(now) {
            self.insert.orphan_exact = orphans
                .site
                .zip(self.type_press_ring.exact_run_between(
                    now,
                    orphans.dispatched_at,
                    orphans.delivered_at,
                ))
                .and_then(|((row, col, print_seq), (presses, len))| {
                    let mut keys = [None; ORPHAN_EXACT_KEYS_MAX];
                    for (i, press) in presses.into_iter().take(len).enumerate() {
                        let (pressed_at, glyph) = press?;
                        let key_col = col.checked_add(u16::try_from(i).ok()?)?;
                        key_col.checked_add(1)?;
                        keys[i] = Some(OrphanExactKey {
                            pressed_at,
                            row,
                            col: key_col,
                            glyph,
                            print_seq,
                        });
                    }
                    Some(OrphanExactRun {
                        keys,
                        next: 0,
                        len,
                        insert_col: col,
                    })
                });
            self.type_press_ring.retire_through(orphans.delivered_at);
        }
        if self.insert.orphan_exact.is_some_and(|run| {
            run.current().is_none_or(|key| {
                now.saturating_duration_since(key.pressed_at).as_secs_f32() > IN_FLIGHT_PATIENCE_S
            })
        }) {
            self.insert.orphan_exact = None;
        }
    }

    /// The source of a move whose caret was hidden last frame, first answer winning.
    pub(super) fn hidden_bridge_source(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> Option<(u16, u16)> {
        // A hidden repaint can return from a proved foreign-row park beyond the hide bridge's reach; only the original input row and its paid forward echo claim custody.
        if let (Some(p), Some((row, col))) = (self.held_park, cur)
            && p.cross_row
            && self.park_source_confirmed
            && p.fresh(now)
            && row == p.row
            && col >= p.origin
            && (col == p.origin
                || self.judging_on_rows(p.row, row, |glow| glow.park_return_paid(&p, col, now)))
        {
            return Some((p.landing_row, p.landing));
        }
        if let (Some(shown), Some(((er, ec), _)), Some((cr, cc))) =
            (self.hide_bridge_shown, self.last_visible, cur)
            && (cr, cc) == shown
            && cr == er
            && cc < ec
        {
            return Some((cr, cc));
        }
        let ((r, c), seen) = self.last_visible?;
        if let Some(run) = cur.and_then(|to| self.exact_repaint_run((r, c), to, now, cfg)) {
            if run.origin_row != r {
                let lift = self.exact_hand_prefix((r, c), run.origin_row, now)?;
                self.v2.lift_hand(lift);
                self.lift_hand_row(r, run.origin_row);
            }
            return Some((run.origin_row, c));
        }
        let fresh = now.saturating_duration_since(seen).as_millis() as u64
            <= crate::cursor_trail::HIDE_BRIDGE_MS;
        let typed_fresh = self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH);
        let nav_bridge = hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            && self
                .nav_hint
                .is_some_and(|key| crate::cursor_trail::nav_bridge_admits(key, seen, now));
        let reach =
            crate::cursor_trail::hide_bridge_reach(typed_fresh, nav_bridge, self.pane_columns);
        let plausible = cur.is_some_and(|to| reach.admits((r, c), to));
        let in_flight_echo = cur.is_some_and(|(cr, cc)| {
            self.judging_on_rows(r, cr, |glow| {
                glow.unpaid_typed_echo(r, c, cr, cc, now, cfg, geom)
            })
        });
        let insert_echo =
            cur.is_some_and(|(cr, cc)| self.visible_insert_echo(r, c, cr, cc, now, cfg).is_some());
        (((fresh || nav_bridge) && plausible) || in_flight_echo || insert_echo).then_some((r, c))
    }

    /// Judge the observed caret against its source.
    pub(super) fn judge_observed_caret(
        &mut self,
        spawn_from: Option<(u16, u16)>,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) {
        let declined_hidden_relocation = spawn_from.is_none()
            && self.last.is_none()
            && cur
                .zip(self.last_visible.map(|(cell, _)| cell))
                .is_some_and(|(current, previous)| current != previous);
        let completed_hidden_reappearance =
            self.last.is_none() && cur.is_some() && self.last_visible.is_some();
        let unseeded_visible = self.last.is_none() && self.last_visible.is_none() && cur.is_some();
        if let (Some((pr, pc)), Some((cr, cc))) = (spawn_from, cur)
            && (pr != cr || pc != cc)
        {
            let dist = (cr.abs_diff(pr)).max(cc.abs_diff(pc));
            self.crown_window_ms = if dist <= 1 {
                Self::CROWN_TYPING_MS
            } else if matches!(cfg.style, GlowStyle::Fire) {
                Self::CROWN_MS + 320
            } else {
                Self::CROWN_MS
            };
            self.coalesced_prefix_echo((pr, pc), (cr, cc), now, cfg, geom);
            let exact_orphans = self.orphan_exact_echo((pr, pc), (cr, cc), now, cfg);
            let orphans = self.insert.orphan_exact.take().and_then(|mut run| {
                if exact_orphans == 0 {
                    return None;
                }
                let mut claimed = [None; ORPHAN_EXACT_KEYS_MAX];
                for (i, slot) in claimed.iter_mut().take(exact_orphans).enumerate() {
                    *slot = Some(run.keys.get(run.next + i).copied().flatten()?);
                }
                run.next += exact_orphans;
                if run.next < run.len {
                    let print_seq = self.print_anchor?.2;
                    run.keys[run.next].as_mut()?.print_seq = print_seq;
                    self.insert.orphan_exact = Some(run);
                }
                Some(claimed)
            });
            if let Some(claimed) = orphans {
                for key in claimed.into_iter().flatten() {
                    self.type_press_ring
                        .bank(key.pressed_at, 1, Some(key.glyph));
                }
            }
            let correction = if orphans.is_none() && matches!(cfg.style, GlowStyle::RainbowKitty) {
                self.judging_on_rows(pr, cr, |glow| {
                    glow.merged_correction((pr, pc), (cr, cc), now)
                })
            } else {
                None
            };
            let admitted = match correction {
                Some((mid, slot)) => {
                    self.judge_merged_correction(pr, (pc, mid, cc), slot, now, cfg, geom)
                }
                None => self.spawn(pr, pc, cr, cc, now, cfg, geom, SpawnLane::Visible),
            };
            if let Some(claimed) = orphans {
                for key in claimed.into_iter().flatten() {
                    self.type_press_ring.revoke_at(key.pressed_at);
                }
            }
            if orphans.is_some() && !admitted {
                self.insert.orphan_exact = None;
            }
            if admitted {
                self.last_move = Some(now);
                self.crown_until = Some(now + Duration::from_millis(self.crown_window_ms));
            } else {
                self.last_move = Some(now);
                self.crown_until = None;
                if self.v2.engaged() {
                    self.v2.observe_caret((cr, cc));
                }
            }
        } else if declined_hidden_relocation {
            // Log the refusal when a key asked for it, so a licensed press whose move died here shows in `aterm ctl trail`.
            if self.move_licensed(now)
                && let (Some((origin, _)), Some(target)) = (self.last_visible, cur)
            {
                self.log_decline(now, origin, target, Self::DECLINE_HIDDEN_RELOCATION);
            }
            if let Some(cur) = cur {
                self.retire_declined_hidden_relocation(cur);
            }
        } else if completed_hidden_reappearance {
            self.retire_hidden_movement_provenance(now);
        } else if unseeded_visible {
            self.retire_all_movement_provenance();
        }
    }

    /// The only witness that releases the next keys held out of the generic pool after an unknown insert.
    pub(super) fn orphan_exact_echo(
        &self,
        from: (u16, u16),
        to: (u16, u16),
        now: Instant,
        cfg: &GlowConfig,
    ) -> usize {
        let Some(run) = self.insert.orphan_exact else {
            return 0;
        };
        let Some(key) = run.current() else {
            return 0;
        };
        let width = usize::from(to.1.saturating_sub(from.1));
        if !matches!(cfg.style, GlowStyle::RainbowKitty)
            || from != (key.row, key.col)
            || to.0 != key.row
            || !(1..=ORPHAN_EXACT_KEYS_MAX).contains(&width)
            || width > run.len.saturating_sub(run.next)
            || (width > 1 && self.ctx_alt && !self.blink_fresh(now))
            || !self.type_press_ring.is_empty()
            || !self
                .insert
                .span
                .is_some_and(|span| span.row == key.row && span.col1 == run.insert_col)
            || !self
                .row_prev_meta
                .is_some_and(|m| m.row == key.row && m.caret == key.col && m.at < now)
            || !self
                .row_cur_meta
                .is_some_and(|m| m.row == key.row && m.caret == to.1 && m.at == now)
            || !self
                .print_anchor
                .is_some_and(|(_, _, seq)| seq > key.print_seq && seq != self.print_anchor_seen)
        {
            return 0;
        }
        for prior in run.keys[..run.next].iter().flatten() {
            let col = usize::from(prior.col);
            if self.row_prev.get(col).copied() != Some(prior.glyph)
                || self.row_cur.get(col).copied() != Some(prior.glyph)
            {
                return 0;
            }
        }
        for i in 0..width {
            let Some(next) = run.keys.get(run.next + i).copied().flatten() else {
                return 0;
            };
            let Some(col) = key.col.checked_add(i as u16) else {
                return 0;
            };
            if next.row != key.row
                || next.col != col
                || matches!(next.glyph, ' ' | '\0')
                || self
                    .print_anchor
                    .is_none_or(|(_, _, seq)| seq <= next.print_seq)
                || self.row_prev.get(usize::from(col)).copied().unwrap_or(' ') != ' '
                || self.row_cur.get(usize::from(col)).copied().unwrap_or(' ') != next.glyph
            {
                return 0;
            }
        }
        width
    }

    /// Judge the typed lay awaiting the caret's rest ([`TornLay`]).
    pub(super) fn settle_torn_lay(&mut self, cur: Option<(u16, u16)>, now: Instant) {
        let Some(mut lay) = self.torn_lay.take() else {
            return;
        };
        if !self.v2.engaged() {
            return;
        }
        if let Some((row, _, seq)) = self.print_anchor
            && Some(seq) != lay.anchor_seq
            && row != lay.row
        {
            lay.printed_off_row = true;
        }
        if cur != lay.seen {
            lay.seen = cur;
            lay.moved_at = now;
        } else if now.saturating_duration_since(lay.moved_at).as_secs_f32() >= Self::TORN_REST_S {
            if let Some(at) = cur {
                self.recall_torn_lay(lay, at, now);
            }
            return;
        }
        self.torn_lay = Some(lay);
    }

    /// Recall a typed lay the caret rests on or left of, keylessly, after a print ended off its row.
    pub(super) fn recall_torn_lay(&mut self, lay: TornLay, (row, col): (u16, u16), now: Instant) {
        if !lay.printed_off_row
            || lay.hand_before.is_some_and(|hand| hand.row == lay.row)
            || row != lay.row
            || col >= lay.col1
        {
            return;
        }
        let (mut own, mut foreign) = (false, false);
        for cell in self.v2.ribbon().cells() {
            if cell.row == row && cell.col >= col && !cell.leaving() {
                if (lay.col0..lay.col1).contains(&cell.col) {
                    own = true;
                } else {
                    foreign = true;
                    break;
                }
            }
        }
        if !own || foreign {
            return;
        }
        self.unsettle();
        self.v2.on_event(
            rk::Event::Rewrite {
                row,
                col,
                cells: lay.col1 - col,
            },
            now,
        );
        self.hand_row = lay.hand_before;
        self.unowned_caret = lay.unowned_before;
        self.log_licensed_as(
            now,
            (row, lay.col1),
            (row, col),
            AdmissionRecord::LICENCE_RECALL,
        );
    }

    /// The caret was shown at `cur` this tick (or not shown at all).
    #[inline]
    pub(super) const fn track_shown_caret(&mut self, cur: Option<(u16, u16)>, now: Instant) {
        self.last = cur;
        if let Some(c) = cur {
            self.last_visible = Some((c, now));
            self.hide_bridge_shown = None;
            self.row_shown_meta = None;
        }
    }

    /// The licence: did a human touch the keyboard just now?
    #[inline]
    #[must_use]
    pub fn move_licensed(&self, now: Instant) -> bool {
        self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH) || self.one_shot_licensed(now)
    }

    /// The one-shot classes of [`Self::move_licensed`].
    #[inline]
    pub(super) fn one_shot_licensed(&self, now: Instant) -> bool {
        hint_fresh(self.quench_hint, now, Self::QUENCH_HINT_FRESH)
            || hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            || hint_fresh(self.return_hint, now, Self::RETURN_HINT_FRESH)
            || hint_fresh(self.newline_hint, now, Self::RETURN_HINT_FRESH)
            || hint_fresh(self.user_gesture_hint, now, Self::USER_GESTURE_HINT_FRESH)
    }

    /// The licence at the seam ([`Self::spawn_judged`]'s gate).
    #[inline]
    pub(super) fn seam_licensed(&self, now: Instant, cfg: &GlowConfig) -> bool {
        let stamp = self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH)
            && (!matches!(cfg.style, GlowStyle::RainbowKitty)
                || self.typed_credits_within(now) >= 1);
        stamp || self.one_shot_licensed(now)
    }

    /// This row's remembered print endpoint and brand, with its slot ([`Self::anchor_rows`]: a linear scan of [`ANCHOR_ROWS`] rows).
    #[inline]
    pub(super) fn anchor_row(&self, row: u16) -> Option<(usize, AnchorRow)> {
        self.anchor_rows
            .iter()
            .enumerate()
            .find_map(|(i, entry)| entry.filter(|mem| mem.row == row).map(|mem| (i, mem)))
    }

    /// Write a row's print endpoint and brand back, stamped with the touch clock.
    pub(super) fn remember_anchor_row(&mut self, slot: Option<usize>, mem: AnchorRow) {
        self.anchor_rows_clock = self.anchor_rows_clock.wrapping_add(1);
        let mem = AnchorRow {
            touched: self.anchor_rows_clock,
            ..mem
        };
        let i = slot.unwrap_or_else(|| {
            self.anchor_rows
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.map(|m| m.touched))
                .map_or(0, |(i, _)| i)
        });
        self.anchor_rows[i] = Some(mem);
    }

    /// Move the hide-bridge source to where the caret will be shown after a licensed anchored echo under a hidden caret (Rainbow Kitty only).
    pub(super) fn relocate_hide_bridge_to_anchor(
        &mut self,
        ar: u16,
        pc: u16,
        ac: u16,
        pane_col1: usize,
        now: Instant,
    ) {
        let estimate = self.last_visible.map(|(cell, _)| cell);
        let col = match estimate {
            Some((row, col)) if row == ar && col < pc => col.saturating_add(ac - pc),
            _ => ac,
        };
        let on_grid = u16::try_from(pane_col1.saturating_sub(1)).unwrap_or(u16::MAX);
        self.hide_bridge_shown = self.hide_bridge_shown.or(estimate);
        self.last_visible = Some(((ar, col.min(on_grid)), now));
    }

    /// The glyph at the last cell of the print run ending at `(row, end)`, as this frame's host sampled it.
    pub(super) fn glyph_before(&self, row: u16, end: u16, now: Instant) -> Option<char> {
        if end == 0 {
            return None;
        }
        if let Some(glyph) = self.print_anchor_glyph
            && self
                .print_anchor
                .is_some_and(|(r, c, _)| r == row && c == end)
        {
            return Some(glyph);
        }
        self.probe_glyph_before(row, end, now)
    }

    /// Whether a run the minibuffer gate of [`Self::echo_anchor_pass`] refused on `row` may be the live keys' own echo rather than a status tick.
    #[inline]
    pub(super) const fn refused_below_the_caret(caret: Option<(u16, u16)>, row: u16) -> bool {
        matches!(caret, Some((cr, _)) if row > cr)
    }

    /// Whether the hand is typing on the drawn `caret`'s row ([`Self::hand_row`]).
    #[inline]
    pub(super) const fn hand_types_at_the_caret(&self, caret: Option<(u16, u16)>) -> bool {
        matches!((caret, self.hand_row), (Some((cr, _)), Some(hand)) if hand.row == cr)
    }

    /// Hold the live presses to the hand's row ([`PressCredits::held_through`]) after the hold refused a run below the drawn caret ending on a glyph no live press typed.
    #[inline]
    pub(super) fn hold_the_live_presses(&mut self, now: Instant) {
        self.type_press_ring.hold_through(now);
    }

    /// Run `judge`, a judgment of a move from row `from` to row `to`, with the held presses ([`PressCredits::held_through`]) payable exactly when the move stays on the hand's row ([`Self::hand_row`]).
    #[inline]
    pub(super) fn judging_on_rows<R>(
        &mut self,
        from: u16,
        to: u16,
        judge: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let admit = from == to && self.hand_row.is_some_and(|hand| hand.row == to);
        let prior = std::mem::replace(&mut self.type_press_ring.admit_held, admit);
        let verdict = judge(self);
        self.type_press_ring.admit_held = prior;
        verdict
    }

    fn exact_repaint_run(
        &self,
        from: (u16, u16),
        to: (u16, u16),
        now: Instant,
        cfg: &GlowConfig,
    ) -> Option<ExactTypedRun> {
        if !self.v2.engaged()
            || !matches!(cfg.style, GlowStyle::RainbowKitty)
            || hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            || hint_fresh(self.return_hint, now, Self::RETURN_HINT_FRESH)
            || hint_fresh(self.newline_hint, now, Self::RETURN_HINT_FRESH)
            || hint_fresh(self.quench_hint, now, Self::QUENCH_HINT_FRESH)
            || hint_fresh(self.kill_hint, now, Self::KILL_HINT_FRESH)
            || hint_fresh(self.reflow_hint, now, Self::REFLOW_HINT_FRESH)
            || hint_fresh(self.user_gesture_hint, now, Self::USER_GESTURE_HINT_FRESH)
            || self.insert.fresh(now).is_some()
            || self.last_erase.is_some_and(|erase| erase.owed_at(now))
        {
            return None;
        }
        self.row_inserted_run(from, to, now)
            .or_else(|| self.split_inserted_run(from, to, now))
            .or_else(|| self.rewrapped_typed_run(from, to, now))
    }

    fn row_inserted_run(
        &self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        now: Instant,
    ) -> Option<ExactTypedRun> {
        let shown = self.row_shown_meta?;
        let current = self.row_cur_meta?;
        if self.last.is_some()
            || !(pr == cr || cr.checked_add(1) == Some(pr))
            || cc <= pc
            || shown.row != pr
            || shown.caret != pc
            || shown.at >= now
            || self.hand_row.is_none_or(|hand| hand.row != pr)
            || current.row != cr
            || current.caret != cc
            || current.at != now
            || self
                .print_anchor
                .is_none_or(|(_, _, seq)| seq == self.print_anchor_seen)
        {
            return None;
        }
        let pane_col0 = self.pane_columns.map_or(0, |(col0, _)| col0);
        if pc <= pane_col0
            || !(pane_col0..pc).all(|col| {
                rk::witness::unit_at(&self.row_shown, col)
                    == rk::witness::unit_at(&self.row_cur, col)
            })
            || !self.insert_tail_preserved(pc, cc)
        {
            return None;
        }
        let cells = usize::from(cc - pc);
        if cells > EXACT_COALESCED_PREFIX_MAX {
            return None;
        }
        let glyphs = self.row_cur.get(usize::from(pc)..usize::from(cc))?;
        if glyphs.contains(&'\0') {
            return None;
        }
        let slot = self.type_press_ring.exact_unpaid_run_with_tail_window(
            now,
            shown.at,
            glyphs,
            IN_FLIGHT_PATIENCE_S,
        )?;
        Some(ExactTypedRun {
            origin_row: cr,
            slot,
            source: None,
            landing_start: pc,
            cells,
            rewrap_start: None,
        })
    }

    fn insert_tail_preserved(&self, old_col: u16, new_col: u16) -> bool {
        let Some(tail) = self
            .row_shown
            .get(usize::from(old_col)..self.ink_end_in_pane(&self.row_shown))
            .filter(|tail| tail.len() >= 2)
        else {
            return false;
        };
        let end = usize::from(new_col).checked_add(tail.len());
        if end.and_then(|end| self.row_cur.get(usize::from(new_col)..end)) == Some(tail) {
            return true;
        }
        if !self
            .row_cur_meta
            .is_some_and(|m| m.below == NbrProbe::Probed)
        {
            return false;
        }
        let Some(first) = self
            .row_cur
            .get(usize::from(new_col)..self.ink_end_in_pane(&self.row_cur))
            .filter(|first| !first.is_empty() && first.len() < tail.len())
        else {
            return false;
        };
        if !tail.starts_with(first) {
            return false;
        }
        let pane_col0 = self.pane_columns.map_or(0, |(col0, _)| col0);
        let end = self.ink_end_in_pane(&self.row_below_cur);
        let Some(start) = (usize::from(pane_col0)..end)
            .find(|&col| !matches!(self.row_below_cur[col], ' ' | '\0'))
        else {
            return false;
        };
        [false, true].into_iter().any(|dropped| {
            let split = first.len() + usize::from(dropped);
            (!dropped || tail.get(first.len()) == Some(&' '))
                && tail.get(split..).is_some_and(|rest| {
                    self.row_below_cur.get(start..start + rest.len()) == Some(rest)
                })
        })
    }

    fn exact_hand_prefix(
        &self,
        (row, col): (u16, u16),
        to: u16,
        now: Instant,
    ) -> Option<rk::witness::FollowRun> {
        let current = self.row_cur_meta?;
        let old_current =
            if row.checked_add(1) == Some(current.row) && current.above == NbrProbe::Probed {
                &self.row_above_cur
            } else if current.row.checked_add(1) == Some(row) && current.below == NbrProbe::Probed {
                &self.row_below_cur
            } else if current.row == row {
                &self.row_cur
            } else {
                return None;
            };
        let width = self.v2.ribbon().typed_suffix_len((row, col), now);
        let lo = col.checked_sub(width).filter(|_| width > 0)?;
        if !(lo..col).any(|c| {
            rk::witness::unit_at(&self.row_shown, c) != rk::witness::unit_at(old_current, c)
        }) {
            return None;
        }
        let hi = col.checked_sub(1)?;
        let last = self
            .v2
            .ribbon()
            .cells()
            .iter()
            .filter(|c| c.row == row && c.col == hi && c.typing && !c.leaving())
            .max_by_key(|c| c.born)?;
        Some(rk::witness::FollowRun {
            row,
            cohort: last.cohort,
            lo,
            hi,
            dr: i16::try_from(i32::from(to) - i32::from(row)).ok()?,
        })
    }

    fn split_inserted_run(
        &self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        now: Instant,
    ) -> Option<ExactTypedRun> {
        let source_row = cr.checked_sub(1)?;
        let (old_row, previous) = if self.last.is_some() {
            (
                &self.row_prev,
                self.row_prev_meta
                    .filter(|probe| probe.row == pr && probe.caret == pc)?,
            )
        } else {
            (&self.row_shown, self.row_shown_meta?)
        };
        let current = self.row_cur_meta?;
        if previous.caret != pc
            || ![previous.row, source_row].contains(&pr)
            || ![source_row, cr].contains(&previous.row)
            || self.hand_row.is_none_or(|hand| hand.row != previous.row)
            || previous.at >= now
            || current.row != cr
            || current.caret != cc
            || current.at != now
            || current.above != NbrProbe::Probed
            || self
                .print_anchor
                .is_none_or(|(_, _, seq)| seq == self.print_anchor_seen)
        {
            return None;
        }
        let pane_col0 = self.pane_columns.map_or(0, |(col0, _)| col0);
        if pc <= pane_col0
            || !(pane_col0..pc).all(|col| {
                rk::witness::unit_at(old_row, col) == rk::witness::unit_at(&self.row_above_cur, col)
            })
        {
            return None;
        }
        let old_end = self.ink_end_in_pane(old_row);
        let old_tail = old_row.get(usize::from(pc)..old_end)?;
        if old_tail.len() < 2
            || self
                .row_cur
                .get(usize::from(cc)..usize::from(cc).checked_add(old_tail.len())?)
                != Some(old_tail)
        {
            return None;
        }
        let landing_start = (pane_col0..cc)
            .find(|&col| !matches!(rk::witness::unit_at(&self.row_cur, col).ch, ' ' | '\0'))?;
        let source_end = u16::try_from(self.ink_end_in_pane(&self.row_above_cur)).ok()?;
        let source_len = usize::from(source_end.checked_sub(pc)?);
        let landing_len = usize::from(cc - landing_start);
        if source_len == 0 || source_len.checked_add(landing_len)? > EXACT_COALESCED_PREFIX_MAX {
            return None;
        }
        let mut glyphs = [' '; EXACT_COALESCED_PREFIX_MAX];
        glyphs[..source_len].copy_from_slice(
            self.row_above_cur
                .get(usize::from(pc)..usize::from(source_end))?,
        );
        for dropped_space in [false, true] {
            let landing_at = source_len + usize::from(dropped_space);
            let cells = landing_at.checked_add(landing_len)?;
            if cells > glyphs.len() {
                continue;
            }
            if dropped_space {
                glyphs[source_len] = ' ';
            }
            glyphs[landing_at..cells].copy_from_slice(
                self.row_cur
                    .get(usize::from(landing_start)..usize::from(cc))?,
            );
            if glyphs[..cells].contains(&'\0') {
                return None;
            }
            if let Some(slot) = self.type_press_ring.exact_unpaid_run_with_tail_window(
                now,
                previous.at,
                &glyphs[..cells],
                IN_FLIGHT_PATIENCE_S,
            ) {
                return Some(ExactTypedRun {
                    origin_row: source_row,
                    slot,
                    source: Some((source_row, pc, source_end)),
                    landing_start,
                    cells,
                    rewrap_start: Some((landing_start, 0)),
                });
            }
        }
        None
    }

    /// The glyph at `(row, end - 1)` in this frame's row probe (the caret's row, [`Self::observe_row`], or a flanking row, [`Self::observe_neighbor_rows`]).
    pub(super) fn probe_glyph_before(&self, row: u16, end: u16, now: Instant) -> Option<char> {
        let col = end.checked_sub(1)?;
        let meta = self.row_cur_meta.filter(|m| m.at == now)?;
        let cols = if row == meta.row {
            &self.row_cur
        } else if row.checked_add(1) == Some(meta.row) && meta.above == NbrProbe::Probed {
            &self.row_above_cur
        } else if meta.row.checked_add(1) == Some(row) && meta.below == NbrProbe::Probed {
            &self.row_below_cur
        } else {
            return None;
        };
        Some(cols.get(usize::from(col)).copied().unwrap_or(' '))
    }

    /// A new glyph immediately before the observed caret, backed by the oldest eligible one-cell press and a resident hand-owned cell.
    pub(super) fn exact_pending_prefix_slot(
        &self,
        row: u16,
        col: u16,
        observed_caret: u16,
        now: Instant,
        same_caret: bool,
    ) -> Option<usize> {
        if col == 0
            || self
                .print_anchor
                .is_none_or(|(_, _, seq)| seq == self.print_anchor_seen)
            || (!same_caret && !self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH))
            || self.typed_credits_within(now) == 0
            || !self
                .row_prev_meta
                .is_some_and(|p| p.row == row && p.caret == col)
            || !self
                .row_cur_meta
                .is_some_and(|p| p.row == row && p.caret == observed_caret)
        {
            return None;
        }
        let glyph_col = usize::from(col - 1);
        let old = self.row_prev.get(glyph_col).copied().unwrap_or(' ');
        let new = self.row_cur.get(glyph_col).copied().unwrap_or(' ');
        if old != ' ' || matches!(new, ' ' | '\0') {
            return None;
        }
        let slot = if same_caret {
            self.type_press_ring.exact_unpaid_run_with_tail_window(
                now,
                self.row_prev_meta?.at,
                &[new],
                IN_FLIGHT_PATIENCE_S,
            )?
        } else {
            let (slot, expected) = self
                .type_press_ring
                .oldest_fresh_glyph(now, Self::TYPE_HINT_FRESH)?;
            (new == expected).then_some(slot)?
        };
        self.v2
            .ribbon()
            .cells()
            .iter()
            .any(|c| c.row == row && c.col == col - 1 && c.typing && !c.leaving())
            .then_some(slot)
    }

    /// A Backspace whose retreat is still owed and the keys typed since it, rendered as one repaint.
    pub(super) fn merged_correction(
        &self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        now: Instant,
    ) -> Option<(u16, usize)> {
        let erase = self.last_erase.filter(|erase| erase.owed_at(now))?;
        if cr != pr {
            return None;
        }
        let keys = i32::from(erase.owed) + i32::from(cc) - i32::from(pc);
        let keys = usize::try_from(keys)
            .ok()
            .filter(|keys| (1..=EXACT_COALESCED_PREFIX_MAX).contains(keys))?;
        let mid = cc.checked_sub(keys as u16)?;
        let source = |m: &RowProbe| m.row == pr && m.caret == pc && m.at < now;
        let (prev, _) = match self
            .row_shown_meta
            .filter(|m| self.last.is_none() && source(m))
        {
            Some(meta) => (&self.row_shown, meta),
            None => (&self.row_prev, self.row_prev_meta.filter(source)?),
        };
        if !self
            .row_cur_meta
            .is_some_and(|m| m.row == pr && m.caret == cc && m.at == now)
        {
            return None;
        }
        let mut glyphs = [' '; EXACT_COALESCED_PREFIX_MAX];
        let mut changed = false;
        for (i, col) in (mid..cc).enumerate() {
            let new = self.row_cur.get(usize::from(col)).copied().unwrap_or(' ');
            if new == '\0' {
                return None;
            }
            changed |= prev.get(usize::from(col)).copied().unwrap_or(' ') != new;
            glyphs[i] = new;
        }
        if !changed {
            return None;
        }
        let slot = self
            .type_press_ring
            .exact_run_after(now, erase.at, &glyphs[..keys])?;
        Some((mid, slot))
    }

    /// Judge a [`Self::merged_correction`] as the two echoes it holds.
    #[allow(
        clippy::too_many_arguments,
        reason = "the row, the correction's three columns and its slot, plus the judge's clock, config and geometry"
    )]
    pub(super) fn judge_merged_correction(
        &mut self,
        row: u16,
        (from, mid, to): (u16, u16, u16),
        slot: usize,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> bool {
        let credits = self.type_press_ring;
        self.type_press_ring.retire_before_slot(slot);
        let retreat = self.spawn(row, from, row, mid, now, cfg, geom, SpawnLane::Visible);
        let advance = retreat && self.spawn(row, mid, row, to, now, cfg, geom, SpawnLane::Visible);
        if !advance {
            self.type_press_ring = credits;
        }
        retreat || advance
    }

    /// What the observed move `(pr, pc) -> (cr, cc)` is to Rainbow Kitty's Backspace whose retreat is still owed ([`EraseWitness`], [`EraseHop`]), `None` for other styles or with no such erase.
    pub(super) fn erase_hop(
        &self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        now: Instant,
        cfg: &GlowConfig,
    ) -> Option<EraseHop> {
        if !matches!(cfg.style, GlowStyle::RainbowKitty) {
            return None;
        }
        let erase = self.last_erase.filter(|erase| erase.owed_at(now))?;
        if cr != pr || cc == pc {
            return Some(EraseHop::Past);
        }
        if cc < pc {
            return Some(if pc - cc <= erase.owed {
                EraseHop::Retreat(pc - cc)
            } else {
                EraseHop::Past
            });
        }
        let hop = usize::from(cc - pc);
        let before = self.type_press_ring.cells_where(now, |t| t <= erase.at);
        if hop <= before {
            return Some(EraseHop::Ahead);
        }
        let late = hop - before;
        let retired = erase.retired();
        let spelled = late <= retired.len()
            && self
                .row_cur_meta
                .is_some_and(|m| m.row == pr && m.at == now)
            && retired[..late].iter().enumerate().all(|(i, &glyph)| {
                self.row_cur.get(usize::from(cc) - late + i).copied() == Some(glyph)
            });
        Some(if spelled {
            EraseHop::Late(late as u16)
        } else {
            EraseHop::Past
        })
    }

    /// A slipped host frame can coalesce Codex's first wrapped-row `a` with later keys.
    pub(super) fn coalesced_prefix_echo(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) {
        let (pr, pc) = from;
        let (cr, cc) = to;
        if !matches!(cfg.style, GlowStyle::RainbowKitty)
            || pr != cr
            || cc <= pc
            || self.last != Some((pr, pc))
        {
            return;
        }
        let Some(slot) = self
            .exact_pending_prefix_slot(pr, pc, cc, now, false)
            .or_else(|| self.exact_coalesced_prefix_slot(pr, pc, cc, now))
        else {
            return;
        };
        self.admit_exact_prefix(pr, pc, slot, now, cfg, geom);
    }

    /// A whole first composer batch can arrive after its first key aged past the per-key hint, or after wrap-fill was refused.
    pub(super) fn exact_coalesced_prefix_slot(
        &self,
        row: u16,
        col: u16,
        target: u16,
        now: Instant,
    ) -> Option<usize> {
        let width = target.checked_sub(col)?.checked_add(1)?;
        if col == 0
            || !(2..=EXACT_COALESCED_PREFIX_MAX).contains(&usize::from(width))
            || self
                .print_anchor
                .is_none_or(|(_, _, seq)| seq == self.print_anchor_seen)
            || !self
                .row_cur_meta
                .is_some_and(|m| m.row == row && m.caret == target && m.at == now)
            || !self.row_prev_meta.is_some_and(|m| {
                m.row == row
                    && m.caret == col
                    && m.at <= now
                    && self
                        .row_prev
                        .get(usize::from(col - 1))
                        .copied()
                        .unwrap_or(' ')
                        == ' '
            })
        {
            return None;
        }
        let mut glyphs = [' '; EXACT_COALESCED_PREFIX_MAX];
        if let Some(src) = self.row_cur.get(usize::from(col - 1)..) {
            let copy_len = src.len().min(usize::from(width));
            glyphs[..copy_len].copy_from_slice(&src[..copy_len]);
        }
        if matches!(glyphs[0], ' ' | '\0') {
            return None;
        }
        self.type_press_ring.exact_unpaid_run(
            now,
            self.row_prev_meta?.at,
            &glyphs[..usize::from(width)],
        )
    }

    /// A rewrap carried the hand's old word left on its row or down one row and printed queued keys after it.
    pub(super) fn rewrapped_typed_run(
        &self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        now: Instant,
    ) -> Option<ExactTypedRun> {
        let same_row = pr == cr && cc < pc;
        let down_one = pr.checked_add(1) == Some(cr);
        let source = |probe: &RowProbe| probe.row == pr && probe.caret == pc && probe.at < now;
        let (old_row, previous) = match self.row_prev_meta.filter(source) {
            Some(previous) => (&self.row_prev, previous),
            None => (&self.row_shown, self.row_shown_meta.filter(source)?),
        };
        let current = self.row_cur_meta?;
        if !(same_row || down_one)
            || current.row != cr
            || current.caret != cc
            || current.at != now
            || (down_one && current.above != NbrProbe::Probed)
            || self.hand_row.is_none_or(|hand| hand.row != pr)
            || self
                .print_anchor
                .is_none_or(|(_, _, seq)| seq == self.print_anchor_seen)
        {
            return None;
        }
        let width = match self.v2.ribbon().moved_word_len((pr, pc), now) {
            0 => self.v2.ribbon().typed_suffix_len((pr, pc), now),
            width => width,
        };
        let source_start = pc.checked_sub(width)?;
        let source_end = pc.checked_sub(1)?;
        let source_now = if same_row {
            &self.row_cur
        } else {
            &self.row_above_cur
        };
        if width == 0
            || !self.v2.ribbon().cells().iter().any(|cell| {
                cell.row == pr && cell.col == source_end && cell.typing && !cell.leaving()
            })
            || (source_start..pc).any(|col| {
                source_now
                    .get(usize::from(col))
                    .is_some_and(|&glyph| glyph != ' ')
            })
        {
            return None;
        }
        let pane_start = self.pane_columns.map_or(0, |(start, _)| start);
        let start = (pane_start..cc).find(|&col| {
            self.row_cur
                .get(usize::from(col))
                .is_some_and(|&glyph| !matches!(glyph, ' ' | '\0'))
        })?;
        let landing_start = start.checked_add(width)?;
        let cells = usize::from(cc.checked_sub(landing_start)?);
        if !(1..=EXACT_COALESCED_PREFIX_MAX).contains(&cells)
            || old_row.get(usize::from(source_start)..usize::from(pc))?
                != self
                    .row_cur
                    .get(usize::from(start)..usize::from(landing_start))?
        {
            return None;
        }
        let mut glyphs = [' '; EXACT_COALESCED_PREFIX_MAX];
        for (i, col) in (landing_start..cc).enumerate() {
            let glyph = self.row_cur.get(usize::from(col)).copied().unwrap_or(' ');
            if glyph == '\0' {
                return None;
            }
            glyphs[i] = glyph;
        }
        let slot = self.type_press_ring.exact_unpaid_run_with_tail_window(
            now,
            previous.at,
            &glyphs[..cells],
            IN_FLIGHT_PATIENCE_S,
        )?;
        Some(ExactTypedRun {
            origin_row: pr,
            slot,
            source: None,
            landing_start,
            cells,
            rewrap_start: Some((start, width)),
        })
    }

    /// Witness for a one-glyph echo when a TUI redraws the caret back onto its old cell in the same bracket.
    pub(super) fn same_caret_typed_echo(
        &mut self,
        cur: Option<(u16, u16)>,
        cursor_move_observed: bool,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> bool {
        let Some((row, col)) = cur else { return false };
        let reappeared =
            self.last.is_none() && self.last_visible.is_some_and(|(cell, _)| cur == Some(cell));
        if !matches!(cfg.style, GlowStyle::RainbowKitty)
            || cursor_move_observed
            || (self.last != cur && !reappeared)
        {
            return false;
        }
        if let Some((mid, slot)) = self.judging_on_rows(row, row, |glow| {
            glow.merged_correction((row, col), (row, col), now)
        }) {
            return self.judge_merged_correction(row, (col, mid, col), slot, now, cfg, geom);
        }
        if reappeared {
            return false;
        }
        let Some(press_slot) = self.judging_on_rows(row, row, |glow| {
            glow.exact_pending_prefix_slot(row, col, col, now, true)
        }) else {
            return false;
        };
        self.admit_exact_prefix(row, col, press_slot, now, cfg, geom)
    }

    fn admit_exact_prefix(
        &mut self,
        row: u16,
        col: u16,
        press_slot: usize,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> bool {
        let credits = self.type_press_ring;
        let hand = self.hand_row;
        self.type_press_ring.retire_before_slot(press_slot);
        self.hand_row = Some(HandRow::at(row));
        let admitted = self.spawn(row, col - 1, row, col, now, cfg, geom, SpawnLane::Visible);
        if !admitted {
            self.type_press_ring = credits;
            self.hand_row = hand;
        } else {
            self.remember_exact_first_cell(row, col - 1, now);
        }
        admitted
    }

    /// A licensed first-cell admission can take `classify_move`'s re-anchor branch and skip `hand_v2_typed_sweep`.
    pub(super) fn remember_exact_first_cell(&mut self, row: u16, col: u16, now: Instant) {
        if !self
            .row_cur_meta
            .is_some_and(|m| m.row == row && m.at == now)
        {
            return;
        }
        let glyph = self.row_cur.get(usize::from(col)).copied().unwrap_or(' ');
        if matches!(glyph, ' ' | '\0') {
            return;
        }
        if let Some(run) = self.recent_typed_run.as_mut()
            && run.row == row
            && run.col0 == col
            && run.glyphs[0] == glyph
        {
            run.at = now;
            return;
        }
        self.recent_typed_run = Some(self.typed_run_from_probe(row, col, col + 1, now));
    }

    /// Remember a typed fold's landing as the continuation row's first typed run (`RAINBOW-KITTY-V2.md` §44).
    pub(super) fn remember_fold_landing(&mut self, row: u16, col0: u16, col1: u16, now: Instant) {
        if col0 >= col1
            || usize::from(col1 - col0) > RECENT_TYPED_RUN_CAP
            || !self
                .row_cur_meta
                .is_some_and(|m| m.row == row && m.at == now)
        {
            return;
        }
        self.recent_typed_run = Some(self.typed_run_from_probe(row, col0, col1, now));
    }

    fn typed_run_from_probe(&self, row: u16, col0: u16, col1: u16, now: Instant) -> RecentTypedRun {
        let mut glyphs = [' '; RECENT_TYPED_RUN_CAP];
        if let Some(src) = self.row_cur.get(usize::from(col0)..) {
            let copy_len = src.len().min(usize::from(col1 - col0));
            glyphs[..copy_len].copy_from_slice(&src[..copy_len]);
        }
        RecentTypedRun {
            row,
            col0,
            len: col1 - col0,
            glyphs,
            at: now,
            origin: None,
        }
    }

    /// The hidden/parked-caret echo lane.
    pub(super) fn echo_anchor_pass(
        &mut self,
        cur: Option<(u16, u16)>,
        cursor_move_observed: bool,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) {
        let Some((ar, ac, seq)) = self.print_anchor else {
            return;
        };
        if self.v2.engaged()
            && !self.caret_hidden
            && let Some(caret) = self.unowned_caret.as_mut()
            && let Some((row, _)) = cur
            && row == caret.row
            && row != ar
            && self.hand_row.is_none_or(|hand| hand.row != row)
        {
            caret.parked = true;
        }
        if seq == self.print_anchor_seen {
            return;
        }
        self.print_anchor_seen = seq;
        let (slot, prev, mut tainted) = match self.anchor_row(ar) {
            Some((i, mem)) => (Some(i), Some(mem.end), mem.tainted),
            None => (None, None, false),
        };
        let was_tainted = tainted;
        let established_echo_row = self.last_anchor_sweep.is_some_and(|(row, _)| row == ar);
        let pane_col1 = {
            let (col0, cols) = self.pane_span(geom);
            col0.saturating_add(cols)
        };
        let wrap_parked = cur.is_some_and(|(r, c)| {
            r == ar
                && usize::from(c).saturating_add(1) == usize::from(ac)
                && usize::from(ac) == pane_col1
        });
        let licensed_this_tick = self
            .last_licensed_row
            .is_some_and(|(row, at)| row == ar && at == now);
        let in_flight = (established_echo_row || wrap_parked)
            && prev.is_some_and(|pc| {
                ac > pc
                    && self.judging_on_rows(ar, ar, |glow| {
                        glow.unpaid_typed_echo(ar, pc, ar, ac, now, cfg, geom)
                    })
            });
        let younger_than_stamp =
            |at: Instant| now.saturating_duration_since(at).as_secs_f32() <= Self::TYPE_HINT_FRESH;
        let insert_admits = prev.and_then(|pc| {
            let ins = self.insert_echo(ar, pc, ar, ac, now, cfg)?;
            let spoken = self
                .last_anchor_sweep
                .is_some_and(|(row, at)| row != ar && younger_than_stamp(at));
            (!spoken && Self::insert_row_identity(ar, usize::from(ac - pc), ins)).then_some(ins)
        });
        if prev.is_some_and(|pc| ac > pc)
            && !self.move_licensed(now)
            && !established_echo_row
            && !licensed_this_tick
            && insert_admits.is_none()
            && !(wrap_parked && in_flight)
        {
            tainted = true;
        }
        self.remember_anchor_row(
            slot,
            AnchorRow {
                row: ar,
                end: ac,
                tainted,
                touched: 0,
            },
        );
        if cursor_move_observed {
            return;
        }
        let hidden = cur.is_none();
        let parked = cur.is_some_and(|(r, _)| r != ar);
        if !(hidden || parked || wrap_parked) {
            return;
        }
        if let Some(pc) = prev
            && ac < pc
            && let Some(cells) = self.insert_rewrite(ar, pc, ar, ac, now, cfg)
        {
            self.retract_insert(ar, ac, cells, now);
            self.last_move = Some(now);
            self.last_anchor_sweep = Some((ar, now));
            return;
        }
        let typed_fresh = self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH);
        if !typed_fresh && !in_flight && self.insert.fresh(now).is_none() {
            if let Some(pc) = prev
                && ac > pc
                && !was_tainted
            {
                self.remember_refused_hop(ar, pc, ac, now, cfg);
            }
            return;
        }
        let Some(pc) = prev else {
            return;
        };
        let bound = if let Some(ins) = insert_admits {
            Self::RAINBOW_TYPED_SWEEP_MAX.max(self.insert_reach(ins, now))
        } else if established_echo_row {
            Self::RAINBOW_TYPED_SWEEP_MAX
        } else {
            Self::ANCHOR_UNPROVEN_ROW_MAX
        };
        if ac <= pc {
            return;
        }
        if usize::from(ac - pc) > bound {
            // Past the cap with a stamp fresh and no receipt yet: the drop's echo seen a frame before its receipt, remembered for one delivery to claim ([`PendingHop`]).
            if !was_tainted {
                self.remember_refused_hop(ar, pc, ac, now, cfg);
            }
            return;
        }
        let spoken_for = self
            .last_anchor_sweep
            .is_some_and(|(row, at)| row != ar && younger_than_stamp(at))
            || self
                .last_licensed_row
                .is_some_and(|(row, at)| row != ar && younger_than_stamp(at));
        if tainted || spoken_for {
            // Logged when contested (a typed stamp fresh, the in-flight batch, or an insert-width advance); a lone spinner tick is refused silently so it cannot push the drop's verdict out of the ring.
            if typed_fresh || in_flight || insert_admits.is_some() {
                self.log_decline(now, (ar, pc), (ar, ac), Self::DECLINE_PROGRAM_ROW);
            }
            return;
        }
        if !typed_fresh && !in_flight && insert_admits.is_none() {
            return;
        }
        if parked && !self.caret_hidden && insert_admits.is_none() {
            let foreign = self
                .glyph_before(ar, ac, now)
                .and_then(|printed| self.type_press_ring.foreign_to_live(now, printed))
                == Some(true);
            let hand_at_caret = self.hand_types_at_the_caret(cur);
            if foreign || hand_at_caret {
                self.log_decline(now, (ar, pc), (ar, ac), Self::DECLINE_PROGRAM_ROW);
                if foreign && Self::refused_below_the_caret(cur, ar) {
                    if hand_at_caret {
                        self.hold_the_live_presses(now);
                    } else {
                        self.forget_typed_credits(now);
                    }
                }
                return;
            }
        }
        self.crown_window_ms = if ac - pc <= 1 {
            Self::CROWN_TYPING_MS
        } else {
            Self::CROWN_MS
        };
        let lane = SpawnLane::Anchored {
            insert_ok: insert_admits.is_some(),
        };
        if self.spawn(ar, pc, ar, ac, now, cfg, geom, lane) {
            self.last_move = Some(now);
            self.last_anchor_sweep = Some((ar, now));
            if usize::from(ac) == pane_col1 {
                self.wrap_paid_row = Some(ar);
            }
            if hidden && matches!(cfg.style, GlowStyle::RainbowKitty) {
                self.relocate_hide_bridge_to_anchor(ar, pc, ac, pane_col1, now);
            }
        }
    }

    /// Release a held park ([`HeldPark`]) by the move that reached the seam, or leave it for the custody arm and the flush (`None`).
    pub(super) fn release_held_park(
        &mut self,
        lane: SpawnLane,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        now: Instant,
        cfg: &GlowConfig,
    ) -> Option<ParkRelease> {
        let p = self.held_park?;
        if !matches!(cfg.style, GlowStyle::RainbowKitty)
            || !p.fresh(now)
            || (p.cross_row && !self.park_source_confirmed)
        {
            return None;
        }
        match lane {
            SpawnLane::Visible if cr == p.row && pr == p.landing_row && pc == p.landing => {
                if cc > p.origin
                    && self.judging_on_rows(p.row, cr, |glow| {
                        glow.park_return_paid(&p, cc, now)
                            || glow
                                .insert_echo(p.row, p.origin, cr, cc, now, cfg)
                                .is_some()
                    })
                {
                    self.held_park = None;
                    self.in_flight_tally.park_returns += 1;
                    Some(ParkRelease::Return {
                        pr: p.row,
                        pc: p.origin,
                    })
                } else if cc == p.origin && !p.content_followed {
                    self.held_park = None;
                    Some(ParkRelease::Cancelled)
                } else {
                    None
                }
            }
            SpawnLane::Anchored { .. }
                if cr == pr && pr == p.row && pc == p.origin && cc > p.origin =>
            {
                self.held_park = None;
                self.in_flight_tally.park_returns += 1;
                Some(ParkRelease::Return { pr, pc })
            }
            _ => None,
        }
    }

    #[inline]
    #[allow(
        clippy::too_many_arguments,
        reason = "from/to cursor cells + clock + config + geometry; packing them into a struct would only obscure a single internal call site"
    )]
    pub(super) fn spawn(
        &mut self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        lane: SpawnLane,
    ) -> bool {
        self.judging_on_rows(pr, cr, |glow| {
            glow.spawn_on_rows(pr, pc, cr, cc, now, cfg, geom, lane)
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the same observed move `spawn` judges"
    )]
    fn spawn_on_rows(
        &mut self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        lane: SpawnLane,
    ) -> bool {
        let (pr, pc) = match self.release_held_park(lane, (pr, pc), (cr, cc), now, cfg) {
            Some(ParkRelease::Cancelled) => return false,
            Some(ParkRelease::Return { pr, pc }) => {
                return self.judging_on_rows(pr, cr, |glow| {
                    glow.spawn_judged(pr, pc, cr, cc, now, cfg, geom, lane, SpawnCall::Live)
                });
            }
            None => (pr, pc),
        };
        if let Some(mut p) = self.held_park
            && p.cross_row
            && p.fresh(now)
        {
            if lane == SpawnLane::Visible {
                p.landing_row = cr;
                p.landing = cc;
                self.held_park = Some(p);
            }
            return true;
        }
        if let Some(p) = self.held_park.take() {
            self.flush_park(p);
        }
        self.spawn_judged(pr, pc, cr, cc, now, cfg, geom, lane, SpawnCall::Live)
    }

    /// Judge a held park ([`HeldPark`]) through the ordinary body at the park's own clock, under the config and geometry it arrived with.
    pub(super) fn flush_park(&mut self, p: HeldPark) {
        self.in_flight_tally.park_flushed += 1;
        if p.cross_row {
            // This landing had positive evidence of being a background park.
            if self.v2.engaged() {
                self.v2.observe_caret((p.landing_row, p.landing));
            }
            self.park_source_cells.clear();
            self.park_source_confirmed = false;
            return;
        }
        let later_stamps = self.type_hint.split_off_after(p.at);
        let reanchor = p.content_followed
            || (p.landing_row == p.row
                && p.origin > p.landing
                && p.origin - p.landing >= rk::ribbon::RE_ANCHOR_MIN_CELLS);
        self.v2.set_flush_clock(reanchor.then_some(p.at));
        let licensed = self.judging_on_rows(p.row, p.landing_row, |glow| {
            glow.with_presses_banked_through(p.at, |glow| {
                glow.spawn_judged(
                    p.row,
                    p.origin,
                    p.landing_row,
                    p.landing,
                    p.at,
                    &p.cfg,
                    p.geom,
                    SpawnLane::Visible,
                    SpawnCall::FlushedPark {
                        landing_glyph: p.landing_glyph,
                        landing_cell: p.landing_cell,
                        content_followed: p.content_followed,
                    },
                )
            })
        });
        if licensed && p.content_followed {
            self.v2
                .prove_short_reanchor((p.row, p.origin), (p.landing_row, p.landing), p.at);
        }
        self.v2.set_flush_clock(None);
        self.type_hint.merge(later_stamps);
    }

    /// Run `judge` against the press ring as it stood at `at`.
    #[inline]
    pub(super) fn with_presses_banked_through<R>(
        &mut self,
        at: Instant,
        judge: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let later = self.type_press_ring.split_off_after(at);
        let r = judge(self);
        self.type_press_ring.merge(later);
        r
    }

    /// Hold a move as a park ([`HeldPark`]) with its config, geometry and whether a glyph sits at its landing cell.
    pub(super) fn hold_park(
        &mut self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) {
        self.held_park = Some(HeldPark {
            row: pr,
            landing_row: cr,
            cross_row: cr != pr,
            origin: pc,
            landing: cc,
            at: now,
            cfg: *cfg,
            geom,
            landing_glyph: self.landing_glyph_at(cr, cc),
            landing_cell: self.probe_glyph_before(cr, cc, now),
            content_followed: false,
        });
    }

    /// The clock a swept cell is born at.
    #[inline]
    pub(super) fn sweep_born(key_at: Instant, now: Instant) -> Instant {
        key_at.max(
            now.checked_sub(Duration::from_secs_f32(Self::TYPE_HINT_FRESH))
                .unwrap_or(now),
        )
    }

    /// Whether a glyph sits left of `col` on `row` in this frame's row probe ([`HeldPark::landing_glyph`]).
    #[inline]
    pub(super) fn landing_glyph_at(&self, row: u16, col: u16) -> bool {
        self.row_cur_meta.is_none_or(|m| {
            m.row != row
                || col
                    .checked_sub(1)
                    .is_some_and(|left| !rk::witness::unit_at(&self.row_cur, left).is_blank())
        })
    }

    /// Whether presses banked at or before the park pay for the return's cells `origin..cc` ([`HeldPark`]).
    #[inline]
    pub(super) fn park_return_paid(&self, p: &HeldPark, cc: u16, now: Instant) -> bool {
        let cells = usize::from(cc.saturating_sub(p.origin));
        let before = if p.cross_row {
            self.typed_credits_within(now)
        } else {
            self.type_press_ring.cells_where(now, |t| t <= p.at)
        };
        before >= cells || Self::share_rule(before, before, cells)
    }

    /// Flush a held park before an edge that would have judged it (a class change, a scroll or band move carrying it off, a stale hidden boundary, a teardown), so a park never outlives its row.
    #[inline]
    pub(super) fn flush_held_park(&mut self) {
        if let Some(p) = self.held_park.take() {
            self.flush_park(p);
        }
    }

    /// Whether a same-row backward move is a park candidate ([`HeldPark`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "the observed move, its clock, style and pane geometry determine park eligibility"
    )]
    pub(super) fn park_candidate(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> bool {
        if !matches!(cfg.style, GlowStyle::RainbowKitty) {
            return false;
        }
        if cr == pr {
            // A lifted word is the composer's re-wrap, not Ink's park; no return follows ([`Self::lifted_word`]).
            if cc >= pc
                || self.lifted_word(pr, pc, cr, cc).is_some()
                || matches!(
                    self.erase_hop((pr, pc), (cr, cc), now, cfg),
                    Some(EraseHop::Retreat(_))
                )
            {
                return false;
            }
        } else if pr.abs_diff(cr) > CROSS_ROW_PARK_ROWS_MAX
            || (pr.abs_diff(cr) > 1 && !self.landing_ends_a_print(cr, cc))
            || self.fold_shape(pr, pc, cr, cc, geom).is_some()
            || self
                .coalesced_fold_shape(pr, pc, cr, cc, now, geom)
                .is_some()
            || self.park_source_intact != Some((pr, pc))
        {
            return false;
        }
        if self.one_shot_licensed(now)
            || hint_fresh(self.kill_hint, now, Self::KILL_HINT_FRESH)
            || hint_fresh(self.reflow_hint, now, Self::REFLOW_HINT_FRESH)
        {
            return false;
        }
        self.typed_credits_within(now) >= 1 || self.insert.fresh(now).is_some()
    }

    /// Whether the caret stands where a print run newly observed by this tick ended ([`Self::observe_print_anchor`], one past its last glyph).
    #[inline]
    pub(super) const fn landing_ends_a_print(&self, row: u16, col: u16) -> bool {
        self.print_in_tick && matches!(self.print_anchor, Some((r, c, _)) if r == row && c == col)
    }

    /// A cross-row park needs content evidence.
    pub(super) fn park_source_unchanged(&self, row: u16, col: u16) -> bool {
        if col == 0
            || !self
                .row_prev_meta
                .is_some_and(|p| p.row == row && p.caret == col)
        {
            return false;
        }
        let Some(sample) = self.witness_rows[..self.witness_rows_n]
            .iter()
            .find(|s| s.row == row)
        else {
            return false;
        };
        let mut ink = false;
        for c in 0..col {
            let previous = rk::witness::unit_at(&self.row_prev, c);
            if previous != rk::witness::unit_at(&sample.cols, c) {
                return false;
            }
            ink |= !previous.is_blank();
        }
        ink
    }

    /// The key pushed the row's text down with the caret.
    pub(super) fn key_pushed_text_down(
        &self,
        (row, col): (u16, u16),
        cur: Option<(u16, u16)>,
        now: Instant,
    ) -> bool {
        let Some((next_row, next_col)) = cur else {
            return false;
        };
        if Some(next_row) != row.checked_add(1)
            || !self
                .row_prev_meta
                .is_some_and(|p| p.row == row && p.caret == col)
            || !self
                .row_cur_meta
                .is_some_and(|m| m.row == next_row && m.caret == next_col)
        {
            return false;
        }
        let Some(left) = next_col.checked_sub(1) else {
            return false;
        };
        let glyph = rk::witness::unit_at(&self.row_cur, left);
        if glyph.is_blank() {
            return false;
        }
        let Some(sample) = self.witness_rows[..self.witness_rows_n]
            .iter()
            .find(|s| s.row == row)
        else {
            return false;
        };
        let old_end = u16::try_from(self.ink_end_in_pane(&self.row_prev)).unwrap_or(u16::MAX);
        let Some(old_tail) = self.row_prev.get(usize::from(col)..usize::from(old_end)) else {
            return false;
        };
        let dest_start = usize::from(next_col);
        let moved_tail = dest_start
            .checked_add(old_tail.len())
            .and_then(|end| self.row_cur.get(dest_start..end));
        old_tail.len() >= 2
            && moved_tail == Some(old_tail)
            && (col..old_end).all(|c| rk::witness::unit_at(&sample.cols, c).is_blank())
            && self
                .type_press_ring
                .fresh_glyph(now, IN_FLIGHT_PATIENCE_S, glyph.ch)
    }

    /// One past the last glyph of a sampled row inside the focused pane (the whole row when no pane is set).
    #[inline]
    pub(super) fn ink_end_in_pane(&self, cols: &[char]) -> usize {
        let pane_end = self
            .pane_columns
            .map_or(cols.len(), |(c0, n)| usize::from(c0) + usize::from(n))
            .min(cols.len());
        cols[..pane_end]
            .iter()
            .rposition(|&c| c != ' ')
            .map_or(0, |i| i + 1)
    }

    /// The licence class: the classifier's verdict in v2's vocabulary, read by the forget edges for every style.
    #[inline]
    pub(super) fn v2_licence(
        &mut self,
        mv: &MoveCtx,
        kill_retreat: bool,
        return_licensed: bool,
        reflow_licensed: bool,
        gesture_taken: Option<Instant>,
    ) -> rk::Licence {
        if kill_retreat {
            rk::Licence::Typed
        } else if mv.navigation {
            rk::Licence::Nav
        } else if return_licensed {
            rk::Licence::Return
        } else if mv.typing || mv.typed_hinted || mv.deletion || mv.bs_pair || reflow_licensed {
            rk::Licence::Typed
        } else {
            if let Some(at) = gesture_taken
                && self.insert.armed.is_some_and(|i| i.at == at)
            {
                self.insert.clear();
            }
            rk::Licence::Synthetic
        }
    }

    /// The forget edges (the in-flight law).
    #[inline]
    pub(super) fn forget_at_licensed_move(
        &mut self,
        mv: &MoveCtx,
        licence: rk::Licence,
        kill_retreat: bool,
        spare: Option<Instant>,
        now: Instant,
    ) {
        let typed_licence = licence == rk::Licence::Typed && !kill_retreat;
        if !typed_licence
            || (mv.cr != mv.pr && !mv.typed_hinted)
            || mv.credit_starved
            || mv.typed_over_cap
        {
            if let Some(at) = spare {
                self.with_presses_banked_through(at, |glow| glow.forget_typed_credits(now));
            } else {
                self.forget_typed_credits(now);
            }
        }
    }

    /// A typed echo can resume beside cells whose swoosh already removed them.
    pub(super) fn hand_v2_typed_sweep(
        &mut self,
        row: u16,
        col0: u16,
        col1: u16,
        born: Instant,
        now: Instant,
    ) {
        let sample = self
            .row_cur_meta
            .is_some_and(|m| m.row == row && m.at == now)
            && col0 < col1;
        if !sample {
            self.recent_typed_run = None;
            self.v2.on_event(rk::Event::Sweep { row, col0, col1 }, born);
            return;
        }
        let old = self.recent_typed_run.as_ref();
        let joined = old.is_some_and(|run| {
            run.row == row
                && run.end() == col0
                && usize::from(run.len) + usize::from(col1 - col0) <= RECENT_TYPED_RUN_CAP
                && now.saturating_duration_since(run.at).as_secs_f32() <= rk::ribbon::CHAIN_GAP_MAX
                && (run.col0..col0).all(|col| {
                    self.row_cur.get(usize::from(col)).copied().unwrap_or(' ')
                        == run.glyphs[usize::from(col - run.col0)]
                })
        });
        let mut origin = None;
        if joined && let Some(run) = old {
            let (standing, first) = self.typed_run_standing(row, run.col0, col0, now);
            origin = first.map(|(_, o)| o).or(run.origin);
            let missing = standing[..usize::from(run.len)].contains(&false);
            if missing {
                self.v2.on_event(
                    rk::Event::Sweep {
                        row,
                        col0: run.col0,
                        col1: col0,
                    },
                    now,
                );
                if let Some(origin) = origin {
                    self.v2.note_revive(rk::ribbon::Revive {
                        row,
                        col0: run.col0,
                        col1: col0,
                        origin,
                    });
                }
            }
        }
        self.v2.on_event(rk::Event::Sweep { row, col0, col1 }, born);
        if joined {
            if let Some(run) = self.recent_typed_run.as_mut() {
                let start = usize::from(run.len);
                let span_len = usize::from(col1 - col0);
                let dst = &mut run.glyphs[start..start + span_len];
                dst.fill(' ');
                if let Some(src) = self.row_cur.get(usize::from(col0)..) {
                    let copy_len = src.len().min(span_len);
                    dst[..copy_len].copy_from_slice(&src[..copy_len]);
                }
                run.len += col1 - col0;
                run.at = now;
                run.origin = origin;
            }
        } else if usize::from(col1 - col0) <= RECENT_TYPED_RUN_CAP {
            self.recent_typed_run = Some(self.typed_run_from_probe(row, col0, col1, now));
        } else {
            self.recent_typed_run = None;
        }
    }

    fn typed_run_standing(
        &self,
        row: u16,
        col0: u16,
        col1: u16,
        now: Instant,
    ) -> (
        [bool; RECENT_TYPED_RUN_CAP],
        Option<(u16, rk::ribbon::RunOrigin)>,
    ) {
        let ribbon = self.v2.ribbon();
        let mut standing = [false; RECENT_TYPED_RUN_CAP];
        let mut first: Option<(u16, rk::ribbon::RunOrigin)> = None;
        let cohorts = ribbon.cohorts();
        let first_cohort = cohorts.first().map_or(0, |coh| coh.id);
        for cell in ribbon.cells() {
            if cell.row != row
                || !(col0..col1).contains(&cell.col)
                || !cell.typing
                || cell.leaving()
            {
                continue;
            }
            let id_offset = cell.cohort.wrapping_sub(first_cohort);
            let Some(coh) = cohorts
                .binary_search_by_key(&id_offset, |coh| coh.id.wrapping_sub(first_cohort))
                .ok()
                .map(|idx| &cohorts[idx])
            else {
                continue;
            };
            if !coh.leaving_at(now)
                && now
                    .saturating_duration_since(cell.born.max(coh.alive_at))
                    .as_secs_f32()
                    < cell.life_s
                && let Some(slot) = standing.get_mut(usize::from(cell.col - col0))
            {
                *slot = true;
                if first.is_none_or(|(c, _)| cell.col < c) {
                    first = Some((cell.col, rk::ribbon::RunOrigin::of(coh)));
                }
            }
        }
        (standing, first)
    }

    /// Read the run's walk on the frame that lays it (§46), once per drawing tick after the engine replayed the frame's events.
    pub(super) fn read_fresh_run_origin(&mut self, now: Instant) {
        let Some(run) = self.recent_typed_run.as_ref() else {
            return;
        };
        if run.origin.is_some() || run.at != now {
            return;
        }
        let (_, first) = self.typed_run_standing(run.row, run.col0, run.end(), now);
        if let Some((_, origin)) = first
            && let Some(run) = self.recent_typed_run.as_mut()
        {
            run.origin = Some(origin);
        }
    }

    /// Hand v2 the licensed move (seam point 1).
    pub(super) fn hand_v2_the_move(
        &mut self,
        mv: &MoveCtx,
        licence: rk::Licence,
        kill_retreat_at: Option<Instant>,
        call: SpawnCall,
    ) {
        let MoveCtx {
            pr,
            pc,
            cr,
            cc,
            now,
            ..
        } = *mv;
        if let Some(run) = mv.exact_typed_run
            && licence == rk::Licence::Typed
        {
            let born = Self::sweep_born(mv.typed_at.unwrap_or(now), now);
            if run.source.is_some() || run.rewrap_start.is_some() {
                self.recent_typed_run = None;
            }
            if let Some((row, col0, col1)) = run.source {
                self.v2.on_event(rk::Event::Sweep { row, col0, col1 }, born);
            }
            self.hand_v2_typed_sweep(cr, run.landing_start, cc, born, now);
            if let Some((start, width)) = run.rewrap_start {
                let space = (run.landing_start..cc)
                    .rev()
                    .find(|&col| self.row_cur.get(usize::from(col)).copied().unwrap_or(' ') == ' ');
                self.v2
                    .prove_typed_rewrap((pr, pc), (cr, cc), start, width, space);
            }
        } else if mv.typing
            && mv.typed_hinted
            && cr == pr
            && cc > pc
            && (mv.rainbow_coalesce || !mv.re_anchor)
        {
            if licence == rk::Licence::Typed {
                self.hand_v2_typed_sweep(cr, pc, cc, mv.typed_at.unwrap_or(now), now);
            } else {
                self.recent_typed_run = None;
                self.v2.on_event(
                    rk::Event::Sweep {
                        row: cr,
                        col0: pc,
                        col1: cc,
                    },
                    mv.typed_at.unwrap_or(now),
                );
            }
        } else if licence == rk::Licence::Typed
            && cr == pr
            && cc < pc
            && (mv.deletion || mv.bs_pair)
        {
            if let Some(run) = self.recent_typed_run.as_mut() {
                if run.row == cr && cc > run.col0 && cc <= run.end() {
                    run.len = cc - run.col0;
                    run.at = now;
                } else {
                    self.recent_typed_run = None;
                }
            }
        } else if licence != rk::Licence::Typed || cr != pr || cc < pc {
            self.recent_typed_run = None;
        }
        let soft_wrap_paid = mv.soft_wrap.is_some()
            && (mv.re_anchor_lays_landing || (mv.typing && mv.shape_wrap && mv.fold_laid > 0));
        if soft_wrap_paid {
            self.v2.on_event(
                rk::Event::Sweep {
                    row: pr,
                    col0: pc,
                    col1: pc + 1,
                },
                Self::sweep_born(mv.typed_at.unwrap_or(now), now),
            );
        }
        let landing_col0 = u16::try_from(mv.pane_col0).unwrap_or(u16::MAX);
        if mv.re_anchor_lays_landing
            && !soft_wrap_paid
            && mv.lift.is_none()
            && cc > landing_col0
            && !matches!(
                call,
                SpawnCall::FlushedPark {
                    landing_glyph: false,
                    ..
                }
            )
        {
            let born = Self::sweep_born(mv.typed_at.unwrap_or(now), now);
            self.v2.on_event(
                rk::Event::Sweep {
                    row: cr,
                    col0: cc - 1,
                    col1: cc,
                },
                born,
            );
            if licence == rk::Licence::Typed
                && mv.typing
                && mv.typed_hinted
                && cr == pr
                && pc.checked_add(1) == Some(cc)
                && let Some(at_probe) = self.row_cur_meta.filter(|m| m.row == cr).map(|m| m.at)
            {
                self.remember_exact_first_cell(cr, cc - 1, at_probe);
            }
        }
        if mv.typing && mv.shape_wrap && mv.fold_laid > 0 && !soft_wrap_paid {
            let born = Self::sweep_born(mv.typed_at.unwrap_or(now), now);
            let pane_col0 = u16::try_from(mv.pane_col0).unwrap_or(u16::MAX);
            let pane_col1 =
                u16::try_from(mv.pane_col0.saturating_add(mv.pane_cols)).unwrap_or(u16::MAX);
            let laid = u16::try_from(mv.fold_laid).unwrap_or(u16::MAX);
            let landing = laid.min(cc.saturating_sub(pane_col0));
            let origin = (laid - landing).min(pane_col1.saturating_sub(pane_col0));
            if origin > 0 {
                self.v2.on_event(
                    rk::Event::Sweep {
                        row: pr,
                        col0: pane_col1 - origin,
                        col1: pane_col1,
                    },
                    born,
                );
            }
            if landing > 0 {
                self.v2.on_event(
                    rk::Event::Sweep {
                        row: cr,
                        col0: cc - landing,
                        col1: cc,
                    },
                    born,
                );
                if licence == rk::Licence::Typed && mv.typed_hinted {
                    self.remember_fold_landing(cr, cc - landing, cc, now);
                }
            }
        }
        if kill_retreat_at.is_some()
            && cr == pr
            && cc < pc
            && self.kill_reported_to_v2 != kill_retreat_at
        {
            let scope = if self.kill_hint_word {
                rk::KillScope::Word
            } else {
                rk::KillScope::Line
            };
            self.v2.on_event(
                rk::Event::Kill {
                    cells: pc - cc,
                    scope,
                },
                now,
            );
            self.kill_reported_to_v2 = kill_retreat_at;
        }
        if let Some(word_col0) = mv.soft_wrap
            && licence == rk::Licence::Typed
        {
            self.v2.note_soft_wrap(
                rk::ribbon::SoftWrap {
                    origin: (pr, pc),
                    landing: (cr, cc),
                    word_col0,
                },
                mv.typed_at,
            );
        }
        if licence == rk::Licence::Typed
            && let Some(above) = pr.checked_sub(1)
            && let Some(dst) = mv.lift
        {
            self.v2.note_lift(
                rk::ribbon::Lift {
                    origin: (pr, pc),
                    landing: (cr, cc),
                    dst: (above, dst),
                },
                mv.typed_at,
            );
        }
        if licence.is_echo()
            && cr == pr
            && cc > pc
            && let Some(sw) = self.v2.soft_wrap_carry()
            && sw.landing == (pr, pc)
            && self.carried_word_left(sw, cr, cc)
        {
            self.v2.note_reflow(sw, (cr, cc));
        }
        self.v2.on_event(
            rk::Event::Move {
                from: (pr, pc),
                to: (cr, cc),
                licence,
                dir: rk::Dir::of(i32::from(cc) - i32::from(pc), i32::from(cr) - i32::from(pr)),
            },
            now,
        );
        self.spawns += 1;
        if mv.credit_starved {
            self.log_decline(now, (pr, pc), (cr, cc), Self::DECLINE_NO_CREDITS);
        } else {
            self.log_typed_licensed(now, (pr, pc), (cr, cc), mv.in_flight_licence);
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "from/to cursor cells + clock + config + geometry + lane + the call; packing them into a struct would only obscure two internal call sites"
    )]
    pub(super) fn spawn_judged(
        &mut self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        lane: SpawnLane,
        call: SpawnCall,
    ) -> bool {
        // A line pasted with its line break ([`Self::line_insert_echo`]): its echo draws the line and the break's new line in one frame, the caret going down a row to the new line's start.
        if lane == SpawnLane::Visible
            && let Some((ins, end)) = self.line_insert_echo((pr, pc), (cr, cc), now, cfg, geom)
        {
            self.lay_insert(pr, pc, pr, end, ins, now);
            return self.spawn_judged(pr, end, cr, cc, now, cfg, geom, lane, call);
        }
        let insert = match lane {
            SpawnLane::Visible => self.visible_insert_echo(pr, pc, cr, cc, now, cfg),
            SpawnLane::Anchored { insert_ok } => {
                self.insert_echo(pr, pc, cr, cc, now, cfg).filter(|ins| {
                    insert_ok && !self.fresher_class_owns_hop(usize::from(cc - pc), *ins, now)
                })
            }
        };
        if let Some(ins) = insert {
            self.lay_insert(pr, pc, cr, cc, ins, now);
            return true;
        }
        if let Some(cells) = self.insert_rewrite(pr, pc, cr, cc, now, cfg) {
            self.retract_insert(cr, cc, cells, now);
            return true;
        }
        let exact_run = (call == SpawnCall::Live && lane == SpawnLane::Visible)
            .then(|| self.exact_repaint_run((pr, pc), (cr, cc), now, cfg))
            .flatten();
        let park = exact_run.is_none()
            && call == SpawnCall::Live
            && lane == SpawnLane::Visible
            && self.park_candidate(pr, pc, cr, cc, now, cfg, geom);
        if park && cr != pr {
            self.park_source_cells.clear();
            self.park_source_cells
                .extend(self.row_prev.iter().take(usize::from(pc) + 1));
            self.park_source_cells.resize(usize::from(pc) + 1, ' ');
        }
        let erase_licensed = matches!(
            self.erase_hop((pr, pc), (cr, cc), now, cfg),
            Some(EraseHop::Retreat(_) | EraseHop::Late(_))
        );
        if !self.seam_licensed(now, cfg)
            && !erase_licensed
            && !self.unpaid_typed_echo(pr, pc, cr, cc, now, cfg, geom)
            && exact_run.is_none()
        {
            if cr == pr && cc > pc {
                self.remember_refused_hop(cr, pc, cc, now, cfg);
            } else {
                if park {
                    self.hold_park((pr, pc), (cr, cc), now, cfg, geom);
                    return false;
                }
                self.forget_typed_credits(now);
            }
            self.log_decline(now, (pr, pc), (cr, cc), Self::DECLINE_NO_FRESH_HINT);
            return false;
        }
        if park {
            self.hold_park((pr, pc), (cr, cc), now, cfg, geom);
            return true;
        }
        self.unsettle();
        let licensed_row_before = self.last_licensed_row;
        self.last_licensed_row = Some((cr, now));
        self.insert.pending_hop = None;
        let unpaid_before = self.typed_credits_within(now) >= 1;
        let exact_credits = exact_run.map(|_| self.type_press_ring);
        let mv = self.classify_move(pr, pc, cr, cc, now, cfg, geom, lane, call, exact_run);
        let parked_redisplay = self.v2.engaged()
            && lane == SpawnLane::Visible
            && self
                .unowned_caret
                .is_some_and(|caret| caret.parked && caret.row == cr)
            && self.hand_row.is_none_or(|hand| hand.row != cr)
            && exact_run.is_none()
            && (mv.typed_hinted || mv.in_flight_licence);
        if mv.landing_foreign || parked_redisplay {
            if let Some(credits) = exact_credits {
                self.type_press_ring = credits;
            }
            self.last_licensed_row = licensed_row_before;
            if parked_redisplay {
                self.forget_typed_credits(now);
            }
            self.log_decline(now, (pr, pc), (cr, cc), Self::DECLINE_PROGRAM_ROW);
            return false;
        }
        let gap = self.update_typing_thermals(&mv);
        self.retire_abandoned_light(&mv);
        let reflow_licensed =
            take_hint_fresh(&mut self.reflow_hint, now, Self::REFLOW_HINT_FRESH).is_some();
        let glyph_echo = matches!(cfg.style, GlowStyle::RainbowKitty)
            && mv.typed_hinted
            && unpaid_before
            && cr == pr
            && cc > pc;
        let return_taken = if glyph_echo {
            None
        } else {
            take_hint_fresh(&mut self.return_hint, now, Self::RETURN_HINT_FRESH)
        };
        let return_licensed = return_taken.is_some();
        let newline_taken = if glyph_echo {
            None
        } else {
            take_hint_fresh(&mut self.newline_hint, now, Self::RETURN_HINT_FRESH)
        };
        let newline_licensed = newline_taken.is_some();
        let gesture_taken = self.user_gesture_hint.take();
        self.cue_move_sound(&mv);
        let kill_retreat_at = if mv.navigation {
            take_hint_fresh(&mut self.kill_retreat_pending, now, Self::KILL_HINT_FRESH)
        } else {
            None
        };
        let kill_retreat = kill_retreat_at.is_some();
        let licence = self.v2_licence(
            &mv,
            kill_retreat,
            return_licensed || newline_licensed,
            reflow_licensed,
            gesture_taken,
        );
        let spare = match licence {
            rk::Licence::Return => return_taken.or(newline_taken),
            rk::Licence::Nav => mv.nav_taken,
            rk::Licence::Typed => kill_retreat_at,
            _ => None,
        };
        self.forget_at_licensed_move(&mv, licence, kill_retreat, spare, now);
        let typed_lay = self.v2.engaged()
            && licence == rk::Licence::Typed
            && lane == SpawnLane::Visible
            && call == SpawnCall::Live
            && (mv.typed_hinted || mv.in_flight_licence)
            && cr == pr
            && cc > pc;
        if self.v2.engaged()
            && let Some(lay) = self.torn_lay.take()
            && typed_lay
        {
            self.recall_torn_lay(lay, (pr, pc), now);
        }
        let hand_before = self.hand_row;
        let unowned_before = self.unowned_caret;
        if mv.typed_hinted || mv.in_flight_licence {
            self.hand_lands_on(cr);
        }
        if self.v2.engaged() {
            self.hand_v2_the_move(&mv, licence, kill_retreat_at, call);
            if typed_lay {
                let col0 = if mv.rainbow_coalesce || !mv.re_anchor {
                    pc
                } else {
                    cc - 1
                };
                self.torn_lay = Some(TornLay {
                    row: cr,
                    col0,
                    col1: cc,
                    seen: Some((cr, cc)),
                    moved_at: now,
                    hand_before,
                    unowned_before,
                    anchor_seq: self.print_anchor.map(|(_, _, seq)| seq),
                    printed_off_row: false,
                });
            }
            return true;
        }
        let boost = self.birth_boost(&mv);
        let spark_life = self.move_spark_life(&mv, gap);
        let hue_step = match cfg.style {
            GlowStyle::Phaser => RAINBOW_PHASER_HUE_STEP,
            _ => RAINBOW_ROLL_HUE_STEP,
        };
        if mv.typing
            && matches!(cfg.style, GlowStyle::Phaser | GlowStyle::Beam)
            && gap <= Self::PHASER_CHAIN_GAP_MAX
        {
            for s in self.sparks.iter_mut().filter(|s| s.typing) {
                let age = now.saturating_duration_since(s.born).as_secs_f32();
                s.life = s.life.min(age + spark_life);
            }
        }
        let fire_meteor =
            mv.fire && !mv.typing && (mv.dr_abs >= 1 || mv.dc_abs >= Self::METEOR_MIN_COLS);
        let born_before = self.born_geometry_census();
        let hue_advances = self.spawn_move_light(
            &mv,
            gap,
            boost,
            spark_life,
            hue_step,
            fire_meteor,
            reflow_licensed,
        );
        self.hue = (self.hue + hue_step * hue_advances).fract();
        self.spawn_lightning(&mv);
        self.spawn_landing_ring(&mv, fire_meteor);
        self.spawn_burst_particles(&mv, fire_meteor);
        if self.particles.len() > Self::MAX_PARTICLES {
            let drop = self.particles.len() - Self::MAX_PARTICLES;
            self.particles.drain(0..drop);
        }
        self.spawns += 1;
        if self.born_geometry_census() == born_before {
            self.log_decline(
                now,
                (pr, pc),
                (cr, cc),
                if mv.credit_starved {
                    Self::DECLINE_NO_CREDITS
                } else {
                    Self::DECLINE_OFF_SHAPE
                },
            );
        } else {
            self.log_typed_licensed(now, (pr, pc), (cr, cc), mv.in_flight_licence);
        }
        true
    }

    /// Record a licensed typed-class verdict.
    #[inline]
    pub(super) fn log_typed_licensed(
        &mut self,
        at: Instant,
        origin: (u16, u16),
        target: (u16, u16),
        in_flight: bool,
    ) {
        if in_flight {
            self.in_flight_tally.licensed += 1;
            self.log_licensed_as(at, origin, target, AdmissionRecord::LICENCE_IN_FLIGHT);
        } else {
            self.log_licensed_as(at, origin, target, AdmissionRecord::LICENCE_KEY);
        }
    }

    /// How much born geometry the engine holds now.
    #[inline]
    pub(super) fn born_geometry_census(&self) -> usize {
        self.sparks.len()
            + self.particles.len()
            + self.fire_meteors.len()
            + self.bolts.len()
            + usize::from(self.ring.is_some())
    }

    /// Classify one licensed cursor move (typing vs. jump, with the wrap, re-anchor and coalesced-echo collapses and bounded classifier hints) and feed the fast-glide velocity spine.
    #[allow(
        clippy::too_many_arguments,
        reason = "one internal call classifies the observed endpoints, frame context and optional exact content proof"
    )]
    pub(super) fn classify_move<'a>(
        &mut self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &'a GlowConfig,
        geom: Geom,
        lane: SpawnLane,
        call: SpawnCall,
        exact_typed_run: Option<ExactTypedRun>,
    ) -> MoveCtx<'a> {
        let content_followed = matches!(
            call,
            SpawnCall::FlushedPark {
                content_followed: true,
                ..
            }
        );
        let dr_abs = (cr as i32 - pr as i32).abs();
        let dc_abs = (cc as i32 - pc as i32).abs();
        let raw_dist = dr_abs.max(dc_abs) as f32;
        let fwd = cr == pr && cc > pc;
        let erase_hop = self.erase_hop((pr, pc), (cr, cc), now, cfg);
        let late_erased = match erase_hop {
            Some(EraseHop::Late(cells)) => usize::from(cells),
            _ => 0,
        };
        let wrap_echo_rhythm = self.last_type.is_some_and(|t| {
            now.saturating_duration_since(t).as_secs_f32() <= Self::PHASER_CHAIN_GAP_MAX
        });
        let (pane_col0, pane_cols) = self.pane_span(geom);
        let fresh_stamp = self.type_hint.peek_fresh(now, Self::TYPE_HINT_FRESH);
        let mut typed_at = fresh_stamp;
        let mut in_flight_licence = false;
        if let Some(run) = exact_typed_run {
            typed_at = self.type_press_ring.slots[run.slot].map(|(at, _, _)| at);
            in_flight_licence = fresh_stamp.is_none();
        } else if typed_at.is_none() && self.unpaid_typed_echo(pr, pc, cr, cc, now, cfg, geom) {
            typed_at = self.type_press_ring.oldest_unpaid(now);
            in_flight_licence = typed_at.is_some();
        }
        let tight_fold = self.fold_shape(pr, pc, cr, cc, geom);
        let coalesced_fold = self
            .coalesced_fold_shape(pr, pc, cr, cc, now, geom)
            .filter(|_| wrap_echo_rhythm || in_flight_licence);
        let shape_wrap = tight_fold.is_some() || coalesced_fold.is_some();
        let fold_cells = tight_fold.or(coalesced_fold);
        let typed_pair = typed_at.is_some();
        let owed_retreat = matches!(erase_hop, Some(EraseHop::Retreat(_)));
        let bs_pair = hint_fresh(self.quench_hint, now, Self::QUENCH_HINT_FRESH) || owed_retreat;
        let bs_fwd = bs_pair && !fwd;
        let return_paired = !fwd && hint_fresh(self.return_hint, now, Self::RETURN_HINT_FRESH);
        let newline_paired = !fwd && hint_fresh(self.newline_hint, now, Self::RETURN_HINT_FRESH);
        let blink_fresh = self.blink_fresh(now);
        let glyph_fwd = matches!(cfg.style, GlowStyle::RainbowKitty)
            && fwd
            && (hint_fresh(self.kill_retreat_pending, now, Self::KILL_HINT_FRESH)
                || (dc_abs == 1 && self.typed_credits_within(now) >= 1)
                || (dc_abs >= 2 && self.typed_share_paid(cr, pc, cc, now)));
        let nav_paired = !glyph_fwd && hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH);
        let echo_run = typed_pair
            && !nav_paired
            && !matches!(cfg.style, GlowStyle::RainbowKitty)
            && fwd
            && cc - pc <= crate::cursor_trail::HIDE_BRIDGE_TYPED_MAX_DIST
            && raw_dist > 1.0;
        let re_anchor = (typed_pair || bs_pair)
            && !nav_paired
            && !return_paired
            && !newline_paired
            && !echo_run
            && dr_abs <= 1
            && (raw_dist > 2.0 || (content_followed && cr == pr && pc > cc && pc - cc <= 2))
            && (!self.ctx_alt || blink_fresh);
        let wrap = shape_wrap || re_anchor || exact_typed_run.is_some();
        let typed_key =
            matches!(cfg.style, GlowStyle::RainbowKitty) && typed_pair && !bs_fwd && !nav_paired;
        let typed_fwd = typed_key && fwd && (!self.ctx_alt || blink_fresh);
        let rainbow_coalesce = typed_fwd
            && dc_abs >= 2
            && dc_abs as usize <= Self::RAINBOW_TYPED_SWEEP_MAX
            && self.typed_share_paid(cr, pc, cc, now);
        let credit_starved = typed_fwd
            && !rainbow_coalesce
            && dc_abs >= 2
            && dc_abs as usize <= Self::RAINBOW_TYPED_SWEEP_MAX;
        let typed_over_cap = typed_fwd && dc_abs as usize > Self::RAINBOW_TYPED_SWEEP_MAX;
        let older_batch = rainbow_coalesce
            && fresh_stamp.is_some_and(|stamp| {
                self.type_press_ring.cells_where(now, |t| t < stamp) >= dc_abs as usize
            });
        let typed_at = if older_batch {
            self.type_press_ring.oldest_unpaid(now).or(typed_at)
        } else {
            typed_at
        };
        let lays_typed_cells =
            typed_key && ((fwd && (rainbow_coalesce || dc_abs == 1)) || shape_wrap);
        let re_anchor_landing = typed_key
            && exact_typed_run.is_none()
            && (!in_flight_licence || self.carried_key_move == Some(((pr, pc), (cr, cc))))
            && re_anchor
            && !lays_typed_cells
            && self.typed_credits_within(now) >= 1;
        let soft_wrap = if exact_typed_run.is_none() && (lays_typed_cells || re_anchor_landing) {
            self.soft_wrapped_caret(pr, pc, cr, cc, pane_col0, now)
        } else {
            None
        };
        let lift = if exact_typed_run.is_none() && typed_key && cr == pr && cc < pc {
            self.lifted_word(pr, pc, cr, cc)
        } else {
            None
        };
        let laid_cell_end = match (soft_wrap, lift) {
            (_, Some(_)) => None,
            (Some(_), None) => pc.checked_add(1).map(|end| (pr, end)),
            (None, None) => Some((cr, cc)),
        };
        let witness = match call {
            SpawnCall::Live => {
                laid_cell_end.and_then(|(row, end)| self.probe_glyph_before(row, end, now))
            }
            SpawnCall::FlushedPark { landing_cell, .. } => laid_cell_end
                .filter(|&cell| cell == (cr, cc))
                .and(landing_cell)
                .filter(|&g| g != ' ' && g != '\0'),
        };
        let landing_foreign = re_anchor_landing
            && lane == SpawnLane::Visible
            && witness.is_some_and(|g| self.type_press_ring.foreign_to_live(now, g) == Some(true));
        let re_anchor_lays_landing = re_anchor_landing && !landing_foreign;
        if landing_foreign {
            self.forget_typed_credits(now);
        }
        let soft_wrap = soft_wrap.filter(|_| !landing_foreign);
        let mut fold_laid = 0;
        if let Some(run) = exact_typed_run {
            self.type_press_ring.retire_before_slot(run.slot);
            let _ = self.type_press_ring.spend_counting(now, run.cells);
        } else if lays_typed_cells || re_anchor_lays_landing {
            let credits = self.typed_credits_within(now);
            let laid = if re_anchor_lays_landing || soft_wrap.is_some() {
                1
            } else {
                fold_cells.unwrap_or(dc_abs as usize - late_erased)
            };
            let cells = laid.min(credits);
            if fold_cells.is_some() {
                fold_laid = cells;
            }
            let _ = self.type_press_ring.spend_counting(now, cells);
        }
        if let Some(stamp) = fresh_stamp
            && !(return_paired || bs_fwd)
        {
            let own_press_unpaid =
                older_batch && self.type_press_ring.cells_where(now, |t| t >= stamp) >= 1;
            if !own_press_unpaid {
                let _ = self.type_hint.take_fresh(now, Self::TYPE_HINT_FRESH);
            }
        }
        let dist = if wrap || rainbow_coalesce || echo_run {
            1.0
        } else {
            raw_dist
        };
        let typing = dist <= 1.0;
        let fire = matches!(cfg.style, GlowStyle::Fire);
        let forward = typing && cr == pr && cc as i32 == pc as i32 + 1;
        let quench_taken = late_erased == 0
            && (take_hint_fresh(&mut self.quench_hint, now, Self::QUENCH_HINT_FRESH).is_some()
                || owed_retreat);
        if let (Some(hop), Some(erase)) = (erase_hop, &mut self.last_erase) {
            match hop {
                EraseHop::Late(cells) => {
                    erase.retired.copy_within(usize::from(cells).., 0);
                    erase.retired_n -= cells as u8;
                }
                EraseHop::Ahead => {}
                EraseHop::Retreat(cells) => {
                    erase.retired_n = 0;
                    erase.owed -= cells;
                }
                EraseHop::Past => {
                    erase.retired_n = 0;
                    erase.owed = 0;
                }
            }
        }
        let deletion = typing && quench_taken && !fwd;
        let nav_taken = if nav_paired {
            take_hint_fresh(&mut self.nav_hint, now, Self::NAV_HINT_FRESH)
        } else {
            None
        };
        let navigation = nav_taken.is_some();
        MoveCtx {
            pr,
            pc,
            cr,
            cc,
            now,
            cfg,
            geom,
            dr_abs,
            dc_abs,
            pane_col0,
            pane_cols,
            shape_wrap,
            fold_laid,
            echo_run,
            re_anchor,
            rainbow_coalesce,
            credit_starved,
            typed_over_cap,
            re_anchor_lays_landing,
            exact_typed_run,
            landing_foreign,
            soft_wrap,
            lift,
            in_flight_licence,
            wrap,
            typed_hinted: typed_at.is_some(),
            typed_at,
            bs_pair,
            dist,
            typing,
            fire,
            forward,
            deletion,
            navigation,
            nav_taken,
        }
    }
}
