// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seam: judging each observed caret move. Licences, the park, the
//! hidden-caret bridge and the classification that decides what light a move
//! may mint.

use super::*;

/// Which lane asks [`CursorGlow::spawn`] to judge a move. The delivered-insert
/// arm reads it for row identity: the visible lane's caret is its own witness,
/// while the anchored lane's print anchor is shared by every row a TUI
/// repaints and carries that lane's own verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SpawnLane {
    /// The visible-caret move lane (`tick`): the DEC cursor moved.
    Visible,
    /// The hidden/parked-caret echo lane (`echo_anchor_pass`): the print
    /// anchor advanced; `insert_ok` is that lane's identity verdict for the
    /// delivered insert on this row.
    Anchored { insert_ok: bool },
}

/// How a held park was released by the move that reached the seam
/// ([`CursorGlow::release_held_park`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ParkRelease {
    /// The return: judged as `(pr, pc) -> target`, the park's origin on the
    /// visible lane (its `Move` was withheld, so the mirror is still there),
    /// the move's own source on the anchored lane.
    Return { pr: u16, pc: u16 },
    /// The same-end rewrite: the park is dropped and nothing is judged.
    Cancelled,
}

/// What [`CursorGlow::spawn_judged`] judges: a live move, or a held park
/// flushed at its own clock ([`CursorGlow::flush_park`]). Only a live
/// visible-lane move may be held as a park. A flushed park's re-anchor
/// withholds its landing sweep when no glyph sat at the landing cell
/// ([`HeldPark::landing_glyph`]), and the landing gate witnesses the glyph that
/// sat there ([`HeldPark::landing_cell`]).
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
    /// Expire the seam's holds at the tick, before this tick's move: a held park
    /// past its window is judged now (a park followed by silence still lands
    /// its `Move`, [`HeldPark`]), and presses an unknown insert's hop echoed are
    /// retired once any next-frame echo would have spent them
    /// ([`InsertSeam::retire_orphans`]).
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
                        key_col.checked_add(1)?; // no same-row claim across a wrap
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
            // The generic `+1` path must never read these keys (an ambient
            // different glyph could spend one); only the key whose exact row
            // transition is proved is restored, for that one spawn call.
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

    /// The source of a move whose caret was hidden last frame, first answer
    /// winning: a proved foreign-row park's own return, the estimate's witness
    /// (a same-cell completed boundary), or the last visible cell when the
    /// landing is plausible from it (the ConPTY hide bridge, a fresh nav
    /// witness, the in-flight echo, or the delivered insert's echo). A pure
    /// read; `None` is a declined relocation. Geometry only: the licence gate
    /// still decides admission.
    pub(super) fn hidden_bridge_source(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> Option<(u16, u16)> {
        // A hidden repaint can return from a proved foreign-row park beyond the
        // hide bridge's reach; only the original input row and its paid
        // forward echo claim custody. The return is paid as a move on the
        // park's own row ([`Self::judging_on_rows`]), as on the visible lane.
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
        // The estimate's witness: `last_visible` may be the anchored lane's
        // relocation estimate. A caret shown exactly where last observed, left
        // of that estimate on its row, never moved: a same-cell completed
        // boundary, which the bridge below would decline and wipe the pool.
        if let (Some(shown), Some(((er, ec), _)), Some((cr, cc))) =
            (self.hide_bridge_shown, self.last_visible, cur)
            && (cr, cc) == shown
            && cr == er
            && cc < ec
        {
            return Some((cr, cc));
        }
        let ((r, c), seen) = self.last_visible?;
        // A torn word-wrapped insert can move the input's source line up while
        // its caret returns to the old physical row. Exact preserved prefix,
        // suffix and chronological keys identify its logical source without
        // lending a generic hidden relocation more reach.
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
        // A fresh typed classifier (peeked; `spawn` consumes) widens the reach,
        // since batched echoes hop farther than 2 cells inside one hide window.
        // Type hint only, so the trail engine's twin agrees on the same move.
        let typed_fresh = self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH);
        // A fresh navigation hint outranks the hide window (lockstep with
        // `CursorTrail::tick`): word and line moves have no key-time cue, so
        // their echo is their only sound. The reach is gesture-shaped, bounded
        // around the press (`nav_bridge_source_ok`, `nav_bridge_admits`), never
        // grid-shaped, which would hand program relocations the key's licence.
        let nav_bridge = hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            && self
                .nav_hint
                .is_some_and(|key| crate::cursor_trail::nav_bridge_admits(key, seen, now));
        let reach =
            crate::cursor_trail::hide_bridge_reach(typed_fresh, nav_bridge, self.pane_columns);
        let plausible = cur.is_some_and(|to| reach.admits((r, c), to));
        // The stalled batch's reappearance (Rainbow Kitty only): a same-row
        // forward reappearance past any bridge reach that the in-flight presses
        // pay for is the echo shape, whatever the hide's age.
        let in_flight_echo = cur.is_some_and(|(cr, cc)| {
            self.judging_on_rows(r, cr, |glow| {
                glow.unpaid_typed_echo(r, c, cr, cc, now, cfg, geom)
            })
        });
        // The licensed insert's reappearance (Rainbow Kitty only): a Tab
        // completion or paste whose echo frame was presented torn reappears the
        // insert's width on, witnessed by its delivery.
        let insert_echo =
            cur.is_some_and(|(cr, cc)| self.visible_insert_echo(r, c, cr, cc, now, cfg).is_some());
        // The nav witness is an alternative to `fresh`, not to `plausible`
        // (which it already widened); the in-flight and insert echoes are their
        // own reach-free lanes.
        (((fresh || nav_bridge) && plausible) || in_flight_echo || insert_echo).then_some((r, c))
    }

    /// Judge the observed caret against its source. A real move between two
    /// visible positions (`spawn_from`, bridge included) is judged by `spawn`
    /// on the visible lane, with the crown window sized to its distance. An
    /// unlicensed move only advances the move clock, drops the cursor-relative
    /// crown and tells v2's mirror where the caret is. A hidden→visible landing
    /// the bridge refused is a declined relocation; a reappearance on the
    /// last-seen cell is a completed hidden boundary (stamps, credits, insert
    /// and park spared); a first draw with no source seeds the anchor and
    /// consumes every classifier dark.
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
                // The crown must survive the meteor's flight (≤320 ms) so the
                // arrival flare still has a live crown to erupt through.
                Self::CROWN_MS + 320
            } else {
                Self::CROWN_MS
            };
            self.coalesced_prefix_echo((pr, pc), (cr, cc), now, cfg, geom);
            // An unknown-width insert's queued keys left the generic pool:
            // restore only the exactly observed glyphs for this spawn (a
            // one-cell frame keeps the next key escrowed, any mismatch drops
            // the remainder).
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
                // An unlicensed move mints and clears nothing: sparks, thermals
                // and the wake leave only by decay, `note_scroll` or `reset`.
                self.last_move = Some(now);
                self.crown_until = None;
                // The mirror follows the observed caret: only licensed moves
                // otherwise write it, so after a declined relocation the next
                // key would lay at the stale mirror. `observe_caret` takes
                // effect after this tick's ingest, so a key buffered before the
                // relocation still lays where it was typed.
                if self.v2.engaged() {
                    self.v2.observe_caret((cr, cc));
                }
            }
        } else if declined_hidden_relocation {
            // Log the refusal when a key asked for it, so a licensed press
            // whose move died here shows in `aterm ctl trail`. Program
            // relocations (no fresh licence) stay unlogged.
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
            // A fresh or reset engine has no truthful source cell: seed the
            // anchor but consume every classifier dark, so the next unrelated
            // CUP cannot borrow a pre-first-draw licence.
            self.retire_all_movement_provenance();
            // v2's mirror is not seeded here: a fresh engine lays nothing for
            // a key struck before its first licensed move (`Engine::reset`),
            // and a mirror at `(r, 0)` would fold that key's neighbour onto
            // the row above.
        }
    }

    /// The only witness that releases the next keys held out of the generic
    /// pool after an unknown insert: exactly those consecutive cells, still on
    /// the insert's row, change from blank to their queued glyphs in a frame
    /// with a new PTY print generation (a two-cell hop needs both in one current
    /// and one prior probe, and a later frame must retain the claimed prefix).
    /// Anything else discards the remainder: the one-key proof per key, never a
    /// generic licence for program output. Returns the cells released.
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
        // A later exact key cannot resume a trail whose earlier proved cell
        // was rewritten: both probes must still carry the original glyph.
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

    /// Judge the typed lay awaiting the caret's rest ([`TornLay`]): every
    /// caret move restarts the wait, a print ending off the lay's row is
    /// noted, and after [`Self::TORN_REST_S`] without a move the lay is
    /// judged where the caret stands.
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

    /// Recall a typed lay the caret rests on or left of, keylessly, after a
    /// print ended off its row: the cells at and right of the caret retract as
    /// an insert's rewrite does, and the hand's row reverts, with the doubt
    /// the lay settled ([`TornLay::unowned_before`]). Only a lay off the
    /// hand's row is recalled (zle's search keys are off it; a second key on a
    /// prompt is on it). A live cell there the lay did not lay keeps the whole
    /// lay; no live cell there is no recall.
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

    /// The caret was shown at `cur` this tick (or not shown at all): the
    /// classifier's source follows it, and a shown caret closes the hide
    /// bridge's estimate witness.
    pub(super) fn track_shown_caret(&mut self, cur: Option<(u16, u16)>, now: Instant) {
        self.last = cur;
        if let Some(c) = cur {
            self.last_visible = Some((c, now));
            self.hide_bridge_shown = None;
            self.row_shown_meta = None;
        }
    }

    // ----- spawning -----

    /// The licence: did a human touch the keyboard just now?
    ///
    /// See `docs/design/EFFECTS-LICENSE-REDESIGN.md`. This is the question the
    /// effects engine asks before minting light, answered by the key-hint
    /// stamps the host arms at the input boundary, each under its own
    /// freshness constant (the 0.25 s class).
    ///
    /// Peeks, never consumes: `spawn`'s classifier owns hint consumption (one
    /// hint, one echo). Class-blind: which choreography fires is
    /// [`Self::classify_move`]'s job.
    ///
    /// Not licence terms, though live stamps:
    /// * `reflow_hint` / `blink_hint`: a resize and a TUI's repaint blink are
    ///   not key presses; they stay morphology inputs.
    /// * `kill_hint`: a caret-moving kill arms `nav_hint` and is licensed by
    ///   it; a stationary kill never reaches `spawn`, and its poof comes from
    ///   `tick`'s erase detector.
    ///
    /// A scripted preview is licensed by the same terms:
    /// [`Self::note_synthetic_move`] stamps `user_gesture_hint` and
    /// [`Self::note_synthetic_typed`] stamps `type_hint`.
    #[inline]
    #[must_use]
    pub fn move_licensed(&self, now: Instant) -> bool {
        self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH) || self.one_shot_licensed(now)
    }

    /// The one-shot classes of [`Self::move_licensed`]: every licence term but
    /// the typed stamp bank (Backspace, navigation, Enter, newline, Tab /
    /// scripted gesture).
    #[inline]
    pub(super) fn one_shot_licensed(&self, now: Instant) -> bool {
        hint_fresh(self.quench_hint, now, Self::QUENCH_HINT_FRESH)
            || hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            || hint_fresh(self.return_hint, now, Self::RETURN_HINT_FRESH)
            || hint_fresh(self.newline_hint, now, Self::RETURN_HINT_FRESH)
            || hint_fresh(self.user_gesture_hint, now, Self::USER_GESTURE_HINT_FRESH)
    }

    /// The licence at the seam ([`Self::spawn_judged`]'s gate):
    /// [`Self::move_licensed`] for every style (the classic trail's lockstep
    /// contract), except that under Rainbow Kitty a typed stamp licenses light
    /// only while the press ring still owes a cell. Coalesced echoes break the
    /// stamp bank's one-stamp-per-echo law (a `+3` spends three presses but
    /// pops one stamp), so the credit ring is the ledger consulted. The one-shot
    /// classes carry no credit.
    #[inline]
    pub(super) fn seam_licensed(&self, now: Instant, cfg: &GlowConfig) -> bool {
        let stamp = self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH)
            && (!matches!(cfg.style, GlowStyle::RainbowKitty)
                || self.typed_credits_within(now) >= 1);
        stamp || self.one_shot_licensed(now)
    }

    /// This row's remembered print endpoint and brand, with its slot
    /// ([`Self::anchor_rows`]: a linear scan of [`ANCHOR_ROWS`] rows).
    #[inline]
    pub(super) fn anchor_row(&self, row: u16) -> Option<(usize, AnchorRow)> {
        self.anchor_rows
            .iter()
            .enumerate()
            .find_map(|(i, entry)| entry.filter(|mem| mem.row == row).map(|mem| (i, mem)))
    }

    /// Write a row's print endpoint and brand back, stamped with the touch
    /// clock: in place if the row has a slot, else an empty slot, else the
    /// least recently written row's slot, so a still-ticking status row is
    /// never the one forgotten ([`ANCHOR_ROWS`]).
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

    /// Move the hide-bridge source to where the caret will be shown after a
    /// licensed anchored echo under a hidden caret (Rainbow Kitty only): the
    /// echo is the only witness of where the caret went, and a stale source
    /// would re-judge the whole hidden run as one hop. End-of-line typing shows
    /// at `ac`, clamped to the last column for the pending-wrap print; mid-line
    /// (Ink re-lays the tail) the source advances from the last shown cell by
    /// the echo's width. The last-seen cell is kept beside the estimate
    /// ([`Self::hide_bridge_shown`]): a show exactly there is no move.
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

    /// The glyph at the last cell of the print run ending at `(row, end)`, as
    /// this frame's host sampled it: the anchor's own glyph when handed over
    /// with this anchor ([`Self::observe_print_anchor_glyph`]), else
    /// [`Self::probe_glyph_before`].
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

    /// Whether a run the minibuffer gate of [`Self::echo_anchor_pass`] refused
    /// on `row` may be the live keys' own echo rather than a status tick: the
    /// row is below the drawn `caret` (zle's i-search row, Emacs's echo area); a
    /// row above is a status row. Below the caret the hand decides: off the
    /// caret's row ([`Self::hand_types_at_the_caret`] false) the run is the keys'
    /// echo and their presses are forgotten; on it, the presses are held to the
    /// hand's row ([`Self::hold_the_live_presses`]).
    #[inline]
    pub(super) fn refused_below_the_caret(caret: Option<(u16, u16)>, row: u16) -> bool {
        caret.is_some_and(|(cr, _)| row > cr)
    }

    /// Whether the hand is typing on the drawn `caret`'s row
    /// ([`Self::hand_row`]): its last licensed typed echo landed there and
    /// nothing but glyphs and line edits were pressed since
    /// ([`Self::clear_typed_keeping_hand`]). Another row's run meanwhile is a
    /// program's.
    #[inline]
    pub(super) fn hand_types_at_the_caret(&self, caret: Option<(u16, u16)>) -> bool {
        caret.is_some_and(|(cr, _)| self.hand_row.is_some_and(|hand| hand.row == cr))
    }

    /// Hold the live presses to the hand's row ([`PressCredits::held_through`])
    /// after the hold refused a run below the drawn caret ending on a glyph no
    /// live press typed: the run may be an echo area's (Emacs `C-u 8-`), so the
    /// presses may now pay only for a move on the hand's row
    /// ([`Self::judging_on_rows`]).
    #[inline]
    pub(super) fn hold_the_live_presses(&mut self, now: Instant) {
        self.type_press_ring.hold_through(now);
    }

    /// Run `judge`, a judgment of a move from row `from` to row `to`, with the
    /// held presses ([`PressCredits::held_through`]) payable exactly when the
    /// move stays on the hand's row ([`Self::hand_row`]): the key's echo, a
    /// coalesced batch, an in-row re-wrap, a park's return (judged from the
    /// park's origin on its own row, wherever the caret parked meanwhile:
    /// an Ink park, or a foreign-row park the hand's echo comes back from;
    /// [`Self::release_held_park`]). Folds, hops
    /// or re-anchors off the row, other rows' advances and reads outside a
    /// judgment see only free presses. A keyless `+1` at the caret on the
    /// hand's row is a stalled echo's shape and is paid like one.
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

    /// Exact content can identify a repainted input batch without using a
    /// caret's jump as evidence. Competing input classes keep their own
    /// admission; these proofs concern only the established typing hand.
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

    /// A repainted input line can move upward while its keys advance on that
    /// line. Its whole shown prefix, exact queued insertion and unchanged old
    /// tail identify the hand even when a long torn copy defeated the generic
    /// arrival witness. The tail may wrap onto the following row.
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

    /// The unedited tail of the last shown input, after the current caret.
    /// One word-wrap break can omit a Space between the two sampled rows.
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

    /// Project the retained hand run only after an exact input insertion
    /// proved its new row. The old glyphs must have left their sampled row;
    /// an unchanged copy beside them grants no lift.
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

    /// A mid-line insertion split across two rows. The prior visible probe,
    /// or the retained shown probe through a hide, supplies the owned prefix
    /// that still stands above the new caret. Its entire old suffix follows
    /// the caret unchanged. Everything inserted between them must spell
    /// consecutive unpaid one-cell keys. A word-wrap break may consume one
    /// Space without painting it; a hard wrap consumes none.
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

    /// The glyph at `(row, end - 1)` in this frame's row probe (the caret's row,
    /// [`Self::observe_row`], or a flanking row,
    /// [`Self::observe_neighbor_rows`]): the last cell of a print run ending at
    /// `(row, end)`, a typed re-anchor's landing left of a caret at
    /// `(row, end)`, or a soft-wrapped caret's origin on the row above
    /// ([`Self::soft_wrapped_caret`]). `None` for any other row, a probe from
    /// another frame (including a held park judged at its own clock), or an
    /// uncaptured neighbour. A trimmed row is blank past its end.
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

    /// A new glyph immediately before the observed caret, backed by the oldest
    /// eligible one-cell press and a resident hand-owned cell. A moving prefix
    /// needs the short key hint; the same-caret path may use the full in-flight
    /// patience, since the exact blank-to-glyph change is its only movement
    /// witness. Reads the host's already-sampled probe only on a candidate.
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
        // A wholly blank row is trimmed to an empty probe. Missing trailing
        // columns are blank cells, unlike a wide glyph's `\0` continuation.
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

    /// A Backspace whose retreat is still owed and the keys typed since it,
    /// rendered as one repaint: an app lagging behind the hand draws the state
    /// after `s⌫␣` (Claude Code's composer, the owner's `the▯subtrees`,
    /// 2026-10-16). The caret moves `j − k` for the `k` cells the erases owe
    /// and `j` keys typed since, so a balanced correction leaves it where it
    /// was, and the `j` cells before it spell those keys. Bytes are processed
    /// in order, so the repaint is the erases' retreat and then the keys'
    /// advance ([`Self::judge_merged_correction`]); a typed Space is its
    /// blank. Requires every press typed before the erase paid, the keys
    /// exact one-cell presses, this frame's probe of the row and the last
    /// frame's at the source caret (after a torn repaint, the last that showed
    /// the caret, [`Self::row_shown`]), and a cell among the `j` changed since:
    /// an erase the app has not processed leaves the row as it was, and a lone
    /// Backspace's echo can read like a correction that retyped the glyph it
    /// erased, whose cell keeps its light. Returns the column the retreat
    /// lands on and the slot of the first key.
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

    /// Judge a [`Self::merged_correction`] as the two echoes it holds: the
    /// erases' retreat `from -> mid`, then the retyped keys' advance
    /// `mid -> to`, which their presses pay. A refused advance puts the
    /// presses back.
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

    /// What the observed move `(pr, pc) -> (cr, cc)` is to Rainbow Kitty's
    /// Backspace whose retreat is still owed ([`EraseWitness`], [`EraseHop`]),
    /// `None` for other styles or with no such erase. Their ordinary hint
    /// window stays in lockstep with the classic trail. Bytes are processed
    /// in order: a key typed and erased inside one echo lag (`thes⌫`, `s` unseen
    /// at the Backspace) still prints before
    /// the erase's retreat, and the keys typed since wait behind it. So a
    /// forward hop the presses typed before the erase pay is theirs; past
    /// them, cells printing the retired keys' glyphs are those keys' late
    /// echo, paid by no press; and the erases' own retreat is their echo
    /// however late, never Ink's park while the next key is in flight. A
    /// hop the keys typed since pay (`abc⌫d` drained as `+2`, then `d`) shows
    /// the erase past.
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

    /// A slipped host frame can coalesce Codex's first wrapped-row `a`
    /// with later keys. Re-lay only that exact glyph before judging the
    /// observed forward hop, which retains its original source and licence.
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

    /// A whole first composer batch can arrive after its first key aged past the
    /// per-key hint, or after wrap-fill was refused. Require every visible cell
    /// from the missing left edge through the caret to match consecutive unpaid
    /// exact keys, plus a fresh same-row probe and a newer PTY print
    /// generation. `spawn` spends one credit for the missing cell; the observed
    /// move spends the rest.
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

    /// A rewrap carried the hand's old word left on its row or down one row
    /// and printed queued keys after it. The old word must match the retained
    /// source probe at the landing, disappear from the sampled source row,
    /// and end exactly where the consecutive unpaid glyphs begin.
    /// Returns only observed geometry and the existing ledger's exact match;
    /// the classifier owns the single spend.
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
        // This is an already-owned run whose source visibly disappeared,
        // rather than first ownership inferred from a moving caret. Its exact
        // queued keys remain payable for the ledger's existing patience.
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

    /// A TUI can redraw the caret back onto its old cell after a one-glyph echo
    /// (Codex, for the first glyph on a wrapped composer row, then printing its
    /// status row in the same bracket), so neither the caret nor the last print
    /// anchor identifies the new glyph; the caret-row probes can. Requires an
    /// in-flight unpaid key with its exact glyph, a new PTY print generation,
    /// and that glyph replacing a blank right behind the unchanged caret. The
    /// usual `spawn` licence then spends that one key.
    ///
    /// A balanced correction leaves the caret where it was too: the erase and
    /// the keys typed since, rendered as one repaint
    /// ([`Self::merged_correction`]), judged first. So does one repainted torn:
    /// the caret, withheld through the torn frames, comes back to the cell it
    /// was last shown on, where only a correction is judged.
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

    /// Both prefix witnesses prove the hand's row from content and exact
    /// unpaid keys, rather than from a raw caret move. Restore that ownership
    /// before the ordinary admission; a refusal retains the old hand and bank.
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

    /// A licensed first-cell admission can take `classify_move`'s re-anchor
    /// branch and skip `hand_v2_typed_sweep`; carry the already-laid glyph into
    /// its dormant content run before the next key joins it. Mints nothing.
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

    /// Remember a typed fold's landing as the continuation row's first typed run
    /// (`RAINBOW-KITTY-V2.md` §44): the fold lays those cells through its own
    /// sweep, so without this a later revive would re-lay the row from one
    /// column past them and leave its first glyph dark. The walk origin is read
    /// once the tick has laid the cells ([`Self::read_fresh_run_origin`]).
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

    /// A fresh [`RecentTypedRun`] of `col0..col1` on `row`, its glyphs copied
    /// from this frame's row probe (blank past its end) and its walk not yet
    /// read. The caller bounds the span to [`RECENT_TYPED_RUN_CAP`].
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

    /// The hidden/parked-caret echo lane: a TUI whose repaint leaves the cursor
    /// hidden, or visible but parked off the caret (CUP 1;1 before show), never
    /// presents a cursor move for its echoes. With no cursor move this tick, the
    /// cursor hidden or off the print anchor's row, and a fresh typed stamp, the
    /// print run's endpoint is the mutation site: the row's previous end is the
    /// launch, the new one the landing, judged by [`Self::spawn`]. Same-row
    /// forward advances only, at most [`Self::RAINBOW_TYPED_SWEEP_MAX`] cells. A
    /// fresh stamp is keystroke correlation, not row identity, so a row tainted
    /// ([`AnchorRow::tainted`]) or spoken for by another row's younger echo is
    /// refused ([`Self::DECLINE_PROGRAM_ROW`]) without spending stamps.
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
        // The brand before this advance: a row branded by an earlier keyless
        // advance is a program row whose hops no receipt may claim; a brand
        // from this advance may be a delivered insert's echo whose receipt is
        // just behind the frame ([`PendingHop`]), and the retro-lay lifts it.
        let was_tainted = tainted;
        // Taint: a row seen advancing while no user gesture was fresh is a
        // program row (spinner, timer, token counter), branded before any gate
        // below can return and sticky for the entry's lifetime. Any fresh
        // gesture exempts it (a Tab or Enter can advance the input row without
        // a typed stamp), and so does the established echo row, which only a
        // licensed anchored echo can earn: an undelivered paste advances it
        // keylessly, and branding it would refuse every later echo.
        let established_echo_row = self.last_anchor_sweep.is_some_and(|(row, _)| row == ar);
        // The pending-wrap print: a glyph at the pane's last column leaves the
        // caret there with a deferred wrap, so no cursor move is observed and
        // the print anchor sits one past the caret. The visible caret is the
        // row's own witness, so this shape opens the anchored lane unbranded.
        let pane_col1 = {
            let (col0, cols) = self.pane_span(geom);
            col0.saturating_add(cols)
        };
        let wrap_parked = cur.is_some_and(|(r, c)| {
            r == ar
                && usize::from(c).saturating_add(1) == usize::from(ac)
                && usize::from(ac) == pane_col1
        });
        // The row the visible lane licensed on this tick (a stalled batch paid
        // by the in-flight pool with no stamp fresh), which `move_licensed`
        // cannot see; without it the merged frame would brand the input row.
        let licensed_this_tick = self
            .last_licensed_row
            .is_some_and(|(row, at)| row == ar && at == now);
        // The in-flight batch on the anchored lane: the row's end advances
        // with no stamp fresh and unpaid presses pay for it (a hidden-caret
        // TUI draining a stall). Licensed only on the established echo row (or
        // the pending-wrap shape), since the unpaid pool has no row identity.
        let in_flight = (established_echo_row || wrap_parked)
            && prev.is_some_and(|pc| {
                ac > pc
                    && self.judging_on_rows(ar, ar, |glow| {
                        glow.unpaid_typed_echo(ar, pc, ar, ac, now, cfg, geom)
                    })
            });
        // The delivered insert's row: a drop as the first action on a
        // never-typed input row must not brand it. Its advance is the insert's
        // echo when it fits, no other row's licensed echo is younger than the
        // stamp window, and the row is the insert's by identity.
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
        // A visible cursor move owns this tick's PTY delta (the move lane
        // judged it); judging it here too would spend two stamps per key.
        if cursor_move_observed {
            return;
        }
        let hidden = cur.is_none();
        let parked = cur.is_some_and(|(r, _)| r != ar);
        if !(hidden || parked || wrap_parked) {
            return;
        }
        // The insert's rewrite under a hidden caret: the TUI swapped the
        // inserted text for a placeholder and the row's end pulled back inside
        // the insert's span, keylessly. Checked before the typed gate; a
        // Backspace or kill keeps its own class (`insert_rewrite` refuses while
        // one is fresh), and a retreat with no insert re-seeds the endpoint.
        if let Some(pc) = prev
            && ac < pc
            && let Some(cells) = self.insert_rewrite(ar, pc, ar, ac, now, cfg)
        {
            self.retract_insert(ar, ac, cells, now);
            self.last_move = Some(now);
            self.last_anchor_sweep = Some((ar, now));
            return;
        }
        // Correlation gate: a fresh typed stamp, a fresh delivered insert, or
        // the in-flight batch on the established echo row opens this lane;
        // nothing else, so program output spams no ledger. A forward advance
        // refused here is remembered for one in-flight delivery receipt
        // ([`PendingHop`]).
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
        // The typed coalesce cap, raised to the insert's width plus the typed
        // credits behind it only for the row the insert admits (for every row,
        // a stamp would license a long advance on a status row). A row no
        // licensed anchored echo has proven keeps `ANCHOR_UNPROVEN_ROW_MAX`.
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
            // Past the cap with a stamp fresh and no receipt yet: the drop's
            // echo seen a frame before its receipt, remembered for one
            // delivery to claim ([`PendingHop`]).
            if !was_tainted {
                self.remember_refused_hop(ar, pc, ac, now, cfg);
            }
            return;
        }
        // Row-discrimination refusal: the row is tainted, or spoken for by
        // another row's licensed echo younger than the stamp window. A refused
        // row never reaches `spawn`, so the stamp survives for its input echo.
        // Both lanes are read: `last_anchor_sweep` is written only by the
        // licensed anchored spawn, so an input row judged on the visible lane
        // (Ink's park under a hide) shows only in `last_licensed_row`.
        let spoken_for = self
            .last_anchor_sweep
            .is_some_and(|(row, at)| row != ar && younger_than_stamp(at))
            || self
                .last_licensed_row
                .is_some_and(|(row, at)| row != ar && younger_than_stamp(at));
        if tainted || spoken_for {
            // Logged when contested (a typed stamp fresh, the in-flight batch,
            // or an insert-width advance); a lone spinner tick is refused
            // silently so it cannot push the drop's verdict out of the ring.
            if typed_fresh || in_flight || insert_admits.is_some() {
                self.log_decline(now, (ar, pc), (ar, ac), Self::DECLINE_PROGRAM_ROW);
            }
            return;
        }
        // Only the insert is fresh and this row is not its own: a program
        // row's partial advance inside the window. Silent, spends nothing.
        if !typed_fresh && !in_flight && insert_admits.is_none() {
            return;
        }
        // The minibuffer below a visible caret (zsh's Ctrl-R): zle keeps the
        // caret on the history match and echoes each key on a status row below
        // as `<glyph>_`, so the run ends on its fake cursor, never the key. On a
        // parked visible caret (not the pending-wrap shape or an insert) the
        // lane lays an echo only where the run ends on a live press's own glyph
        // ([`Self::glyph_before`]); a program that parks a visible caret and
        // draws its own fake cursor goes dark here, by design. A refused
        // minibuffer echo forgets its presses; a refused status tick forgets
        // nothing, or the key's real echo would be refused a frame later. While
        // the hand types on the caret's row, an advance on any other row is
        // refused `program-row` (a spinner digit can match the key), and a run
        // below the caret ending on a foreign glyph holds the presses to the
        // hand's row (an Emacs echo area's copy of the key).
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
            // The licensed echo establishes this row as the echo row (the
            // spoken-for hold above).
            self.last_anchor_sweep = Some((ar, now));
            // The pending-wrap print paid the pane's last column: the fold
            // that follows from it carries only the new row's head.
            if usize::from(ac) == pane_col1 {
                self.wrap_paid_row = Some(ar);
            }
            if hidden && matches!(cfg.style, GlowStyle::RainbowKitty) {
                self.relocate_hide_bridge_to_anchor(ar, pc, ac, pane_col1, now);
            }
        }
    }

    /// Release a held park ([`HeldPark`]) by the move that reached the seam, or
    /// leave it for the custody arm and the flush (`None`). Rainbow Kitty only,
    /// inside the park's window (a cross-row park also needs its source
    /// confirmed):
    ///
    /// - the visible-lane return, a forward move from the landing past the
    ///   origin, judged `origin -> target`; paid only by presses banked at or
    ///   before the park ([`Self::park_return_paid`]; vim's `$` after `b` must
    ///   not pay) or by a delivered insert's echo shape. Both are judged on the
    ///   park's own row ([`Self::judging_on_rows`]), so a foreign-row park's
    ///   return to the hand's row is paid as the hand's echo, held presses
    ///   included;
    /// - the anchored-lane return (Ink's hide-rewrite-show sampled mid-bracket),
    ///   the print anchor advancing from the origin, judged `origin -> end`;
    /// - the same-end rewrite, a repaint back to the old end: dropped, and the
    ///   stamp survives for its echo.
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
        // The held presses pay for this move only if it stays on the hand's
        // row; a park's return is judged on the park's own row
        // ([`Self::judging_on_rows`], [`Self::release_held_park`]).
        self.judging_on_rows(pr, cr, |glow| {
            glow.spawn_on_rows(pr, pc, cr, cc, now, cfg, geom, lane)
        })
    }

    /// [`Self::spawn`]'s body, under the held presses' admission.
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
            // The return is the move from the park's origin on its own row
            // ([`Self::judging_on_rows`]), wherever the caret parked.
            Some(ParkRelease::Return { pr, pc }) => {
                return self.judging_on_rows(pr, cr, |glow| {
                    glow.spawn_judged(pr, pc, cr, cc, now, cfg, geom, lane, SpawnCall::Live)
                });
            }
            None => (pr, pc),
        };
        // A fresh cross-row park keeps custody through more foreign-row
        // redraws, so the pending key never turns into footer light: only its
        // observed landing updates. Credit patience bounds the custody.
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
        // The flush: any other move judges the held park first, at the
        // park's own clock — today's verdict, emitted later.
        if let Some(p) = self.held_park.take() {
            self.flush_park(p);
        }
        self.spawn_judged(pr, pc, cr, cc, now, cfg, geom, lane, SpawnCall::Live)
    }

    /// Judge a held park ([`HeldPark`]) through the ordinary body at the park's
    /// own clock, under the config and geometry it arrived with. The licence
    /// gate re-evaluates at `p.at` (an in-flight park takes the refusal branch
    /// and forgets the pool); `classify_move` pops the park's own stamp, lays
    /// nothing forward, and classifies a wide retreat or a content-proved short
    /// wrap as a re-anchor (an unproved one-cell park as `typing`, a two-cell
    /// one as a jump); v2 gets its `Move` dated `p.at`; the ring row is today's.
    pub(super) fn flush_park(&mut self, p: HeldPark) {
        self.in_flight_tally.park_flushed += 1;
        if p.cross_row {
            // This landing had positive evidence of being a background
            // park. Time passing is not a new glyph or a keyboard licence.
            if self.v2.engaged() {
                self.v2.observe_caret((p.landing_row, p.landing));
            }
            self.park_source_cells.clear();
            self.park_source_confirmed = false;
            return;
        }
        // Judged against the banks as they stood at the park: freshness reads
        // treat any `t >= now` as fresh, so a key typed after the park would
        // otherwise license it and lose its own echo. Later stamps and presses
        // are set aside and put back afterwards.
        let later_stamps = self.type_hint.split_off_after(p.at);
        // The events the judgment mints (landing sweep, `Move`) are ordered
        // before keys pressed since (`rk::Engine::set_flush_clock`), but only
        // for a park the ribbon will read as a re-anchor (the composer's
        // box-growth wrap); below the floor the flush is just the mirror
        // moving, and reordering would hold the next key instead of laying it.
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

    /// Run `judge` against the press ring as it stood at `at`: presses banked
    /// after `at` are set aside and put back, so a judgment at a past clock
    /// (a park flush, a Return's or arrow's forget edge) cannot spend a press
    /// typed since. `judge` never banks, so indices are unchanged.
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

    /// Hold a move as a park ([`HeldPark`]) with its config, geometry and
    /// whether a glyph sits at its landing cell.
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

    /// The clock a swept cell is born at: the key's, floored one stamp window
    /// before the echo, so a debounced TUI's late echo is not born mid-retract.
    #[inline]
    pub(super) fn sweep_born(key_at: Instant, now: Instant) -> Instant {
        key_at.max(
            now.checked_sub(Duration::from_secs_f32(Self::TYPE_HINT_FRESH))
                .unwrap_or(now),
        )
    }

    /// Whether a glyph sits left of `col` on `row` in this frame's row probe
    /// ([`HeldPark::landing_glyph`]); the probe is fed before the tick, so at
    /// park time it is the row as the park found it. No probe, or another
    /// row's, reads as a glyph.
    #[inline]
    pub(super) fn landing_glyph_at(&self, row: u16, col: u16) -> bool {
        self.row_cur_meta.is_none_or(|m| {
            m.row != row
                || col
                    .checked_sub(1)
                    .is_some_and(|left| !rk::witness::unit_at(&self.row_cur, left).is_blank())
        })
    }

    /// Whether presses banked at or before the park pay for the return's cells
    /// `origin..cc` ([`HeldPark`]): outright, or under the share rule, so a
    /// rewrite one press short of a burst still returns.
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

    /// Flush a held park before an edge that would have judged it (a class
    /// change, a scroll or band move carrying it off, a stale hidden boundary,
    /// a teardown), so a park never outlives its row.
    #[inline]
    pub(super) fn flush_held_park(&mut self) {
        if let Some(p) = self.held_park.take() {
            self.flush_park(p);
        }
    }

    /// Whether a same-row backward move is a park candidate ([`HeldPark`]):
    /// Rainbow Kitty, the visible lane, no one-shot class fresh
    /// ([`Self::one_shot_licensed`]) nor a kill or reflow, and something in
    /// flight behind it: a press (the key's own, or the stalled pool's, judged
    /// at the flush as the keyless hop it was) or a fresh delivered insert (a
    /// dropped path takes Ink's park + rewrite route too). Any width: Ink parks
    /// by one and two cells before parking wide, and holding changes no
    /// verdict, only defers the v2 `Move` by at most one stamp window. A
    /// cross-row park stands at most [`CROSS_ROW_PARK_ROWS_MAX`] rows from the
    /// row it left, off a fold, with the source it left intact; past the
    /// adjacent row only where the program's print ended
    /// ([`Self::landing_ends_a_print`]).
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
            // A lifted word is the composer's re-wrap, not Ink's park; no
            // return follows ([`Self::lifted_word`]). A retreat the
            // Backspaces still owe is their echo, however late
            // ([`Self::erase_hop`]).
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
        // A press in flight (a fresh stamp with nothing unpaid holds nothing:
        // it is refused like the keyless control) or a delivered insert
        // awaiting its echo.
        self.typed_credits_within(now) >= 1 || self.insert.fresh(now).is_some()
    }

    /// Whether the caret stands where a print run newly observed by this tick ended
    /// ([`Self::observe_print_anchor`], one past its last glyph): the caret a
    /// repaint left on the row it was writing. A footer or status repaint two
    /// or three rows from the composer ends its run there; a motion key's
    /// jump (vim's `}`, `2j`, `3k`, `L`) is a bare cursor move whose landing
    /// no print ended at, so it is judged on its own frame instead of held
    /// ([`Self::park_candidate`]). An unwired host reports no anchor and
    /// holds only the adjacent row's park. A cursor-only jump onto a retained
    /// endpoint from an earlier tick does not supply fresh print evidence.
    #[inline]
    pub(super) fn landing_ends_a_print(&self, row: u16, col: u16) -> bool {
        self.print_in_tick
            && self
                .print_anchor
                .is_some_and(|(r, c, _)| r == row && c == col)
    }

    /// A cross-row park needs content evidence: the exact source prefix sampled
    /// on the preceding frame is still present now (the resident ribbon's row
    /// sample is the second observation). A real wrap, moved input box,
    /// unknown row or changed prefix keeps its immediate verdict.
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

    /// The key pushed the row's text down with the caret: when a key makes the
    /// word at the caret too long for row `r`, Claude Code's Ink erases row `r`
    /// from the caret (`ESC[K`) and writes the key's glyph and the word on the
    /// continuation row. Row `r`'s prefix is untouched, so
    /// [`Self::park_source_unchanged`] alone would hold it as a footer repaint.
    ///
    /// Requires all three: the source row's tail from the origin to its old ink
    /// end was erased this frame, that exact tail follows the landing glyph on
    /// the row below, and the landing glyph is an unpaid press's own. The moved
    /// tail is the content proof (a footer can erase the tail and print the
    /// key's glyph).
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

    /// One past the last glyph of a sampled row inside the focused pane (the
    /// whole row when no pane is set): a split's neighbour is not this
    /// input's text.
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

    /// The licence class: the classifier's verdict in v2's vocabulary, read by
    /// the forget edges for every style. A kill's own caret retreat is `Typed`
    /// (the drain is the `Kill` event); typed echoes, Backspace retreats and a
    /// settled resize are `Typed` too and inert in v2. `bs_pair` was peeked
    /// before `classify_move` consumed the quench hint, so a quench-licensed jump
    /// keeps its class. A `Synthetic` move that took the Tab's gesture spends its
    /// insert stamp with it.
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

    /// The forget edges (the in-flight law): a licensed move the in-flight
    /// presses cannot explain forgets them, mirroring the echo ledger's clears
    /// (`Engine::echo_bridge`): a non-typed licence (arrow, Return, scripted
    /// gesture, kill retreat), a row change no typed stamp paired, a forward hop
    /// the share rule refused, a typed-paired hop past the cap. A Return's,
    /// arrow's, newline's or moving kill's move forgets only presses banked at or
    /// before its own key (`spare`): later glyphs are type-ahead.
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

    /// A typed echo can resume beside cells whose swoosh already removed them:
    /// the last run's exact grid glyphs are kept outside the live ribbon, and
    /// the next licensed adjacent typed echo re-lays them once if any old cell
    /// expired while the row still holds every glyph. The re-lay is named with
    /// the run's walk ([`rk::ribbon::Revive`], §43), so the line returns on its
    /// own stops in either order the frame lays it and the key's cell.
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
        // The run's walk as its standing cells show it on this key frame
        // (`RecentTypedRun::origin`), or as an earlier frame read it.
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
                // Unnamed, a re-lay ahead of the key's cell would continue the
                // retired walk from its end.
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

    /// The typed run `col0..col1` on `row` as the ribbon holds it at `now`
    /// (at most [`RECENT_TYPED_RUN_CAP`] columns): which of its columns
    /// still carry a standing typed cell, and the leftmost such cell's
    /// column with its cohort's walk ([`rk::ribbon::RunOrigin`]).
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
        // The leftmost standing cell's column and its cohort's walk.
        let mut first: Option<(u16, rk::ribbon::RunOrigin)> = None;
        let cohorts = ribbon.cohorts();
        // Cohorts are appended with wrapping IDs and only removed, so offsets
        // from the oldest live ID stay sorted across a wrap; one binary search
        // per cell avoids scanning cohorts.
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

    /// Read the run's walk on the frame that lays it (§46), once per drawing
    /// tick after the engine replayed the frame's events: a typed run begun or
    /// grown this frame with its walk unread reads it off the cells just laid
    /// ([`Self::typed_run_standing`]). Otherwise a run the hand rested after
    /// would have no walk and its revive would continue `last_walk` from the
    /// run's end.
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

    /// Hand v2 the licensed move (seam point 1): the typed sweep, the
    /// re-anchor's landing, the fold's cells, the kill's retreat, then the
    /// `Move` (the ribbon's contract order), and the ring row.
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
        // A typed echo hands v2 its glyph cells ([`rk::Event::Sweep`]), so a
        // batched echo (`rainbow_coalesce`, paid by the press ring) or a
        // frame-late key does not leave cells dark; the ribbon lays only what
        // no live cell owns. A TUI re-anchor (vim's `w`: a stamp beside a long
        // unpaid hop) lights only the landing; the coalesced echo outranks it.
        //
        // The sweep is dated at the key's clock (the stamp, or the oldest
        // unpaid press for a stalled batch), which the echo ledger partitions
        // by; the ribbon floors birth one stamp window before the echo
        // (`rainbow_kitty::SWEEP_BIRTH_FLOOR_S`).
        //
        // Only for a typed-paired hop: `mv.typing` alone (`dist <= 1`) is also
        // met by a one-shot key's +1, and a keyless program +1 after a
        // Backspace or moving kill must not light as typed. An arrow's hop
        // keeps its wake (`Licence::Nav`).
        if let Some(run) = mv.exact_typed_run
            && licence == rk::Licence::Typed
        {
            let born = Self::sweep_born(mv.typed_at.unwrap_or(now), now);
            // Ordinary insertions keep their adjacent run's revival witness;
            // a split or rewrapped prefix changes that glyph geometry.
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
        // A soft-wrapped caret lays its key where the glyph is
        // ([`Self::soft_wrapped_caret`]): the glyph stands at the origin and
        // the landing row's head is the blank indent, so the paid cell is
        // `(pr, pc)`, swept on the landing's clock in its place.
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
        // The re-anchor's landing is swept. A typed re-anchor lays exactly one
        // cell, the landing, and spends its credit on it, but otherwise only
        // the key's `Typed` replay lights it, folding from a caret mirror that
        // a held park may leave behind (the box-growth wrap's key replays
        // against the pre-wrap caret). Swept here on the spend's predicate,
        // dated at the key and floored one stamp window back like the fold's;
        // the ribbon lays only unowned cells, so an already-laid landing is
        // unchanged.
        //
        // The landing folds at the focused pane's first column, as
        // `Ribbon::lay` wraps at `set_pane`'s edges, whereas `Ribbon::sweep`
        // relies on the grid and would lay into a neighbouring split. A landing
        // on the pane's first column is refused rather than folded: the credit
        // is spent and no light minted. A flushed park sweeps its landing only
        // if a glyph sat there when held (an Ink park beside the prompt must
        // not light).
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
            // A one-cell typed re-anchor can lay the first wrapped-row glyph
            // without the typed-Sweep path. Keep its licensed content for a
            // later same-row continuation.
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
        // The fold's own cells (the stalled key whose echo wraps the row): a
        // typed fold reaches v2 as an inert `Move`, and a stalled key misses
        // the `Typed` replay that lights a per-key fold, so the seam sweeps the
        // origin row's tail and the landing row's head, exactly the cells the
        // fold paid for (`fold_laid`), dated at the key.
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
                // The landing starts the continuation row's typed run, so a
                // revive after a rest re-lays the row from its first glyph
                // ([`Self::remember_fold_landing`], §44).
                if licence == rk::Licence::Typed && mv.typed_hinted {
                    self.remember_fold_landing(cr, cc - landing, cc, now);
                }
            }
        }
        // The kill's retreat carries its kill: emitted before the `Move`, so
        // the ribbon takes the erase-retreat arm and drains exactly the
        // retreated cells (otherwise `re_anchor` would relay the killed text's
        // last word). One kill, one `Kill` (`kill_reported_to_v2`), whichever
        // of this and the poof mints it first.
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
        // The engine is told before the `Move` it names: its replay lays a
        // key echoed on this frame at the origin rather than the frame's
        // caret, and the ribbon keeps the word the key ends on the origin
        // row for the key that reflows it (`Engine::note_soft_wrap`).
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
        // A lifted word ([`Self::lifted_word`]): the ribbon carries the word's
        // light up with its text.
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
        // The soft-wrapped word came down ([`Self::carried_word_left`]): the
        // ribbon relays the word only on the echo that carried it off.
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
        // Scored for `trail status` as a v1 spawn is (the v1 geometry census
        // cannot see v2's births).
        self.spawns += 1;
        // The move is licensed and its landing laid, but when the credit
        // budget alone refused the coalesce the typed cells were not born, and
        // the ring records a refusal.
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
        // A line pasted with its line break ([`Self::line_insert_echo`]): its
        // echo draws the line and the break's new line in one frame, the caret
        // going down a row to the new line's start. The insert pays for its
        // line, laid as its sweep on its own row; the rest of the move, from the
        // line's end to the new line, is judged as any other (the line's band
        // then leaves at its rest, as a typed line's does when the hand goes
        // down a line), and its verdict is the move's.
        if lane == SpawnLane::Visible
            && let Some((ins, end)) = self.line_insert_echo((pr, pc), (cr, cc), now, cfg, geom)
        {
            self.lay_insert(pr, pc, pr, end, ins, now);
            return self.spawn_judged(pr, end, cr, cc, now, cfg, geom, lane, call);
        }
        // The delivered insert (Rainbow Kitty only; `move_licensed` stays the
        // classic trail's lockstep contract). A file drop, ⌘V, the `paste`
        // verb, a Tab completion or ⌃V is the user's gesture, but its echo is
        // program output to the PTY stream, and its gesture stamp is revoked at
        // enqueue. The witness is the host's completed write
        // (`note_insert_delivered`): a same-row forward hop no wider than the
        // insert plus the typed credits behind it, on its own row
        // (`insert_row_identity`), unexplained by a fresher press class, is laid
        // as one sweep joining the cohort and nothing else (an insert is not
        // typing). Judged before the licence gate, whose classes cannot
        // describe it, and before `classify_move`, which would read a long hop
        // beside a typed stamp as a re-anchor.
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
        // The insert's rewrite: the program pulled the caret back inside an
        // insert's span keylessly (Claude Code's `[Image #1] ` swap), the row
        // probe showing the suffix blank. Judged before the typed classifier,
        // which would call it a re-anchor and retract nothing.
        if let Some(cells) = self.insert_rewrite(pr, pc, cr, cc, now, cfg) {
            self.retract_insert(cr, cc, cells, now);
            return true;
        }
        // The licence seam (see [`Self::move_licensed`]). An unlicensed move
        // (program output nobody asked for) returns here, before
        // `classify_move` and before any state moves:
        //
        // Mint nothing: no thermal, sound cue, birth, glide sample or crown.
        //
        // Clear nothing: earned light is never destroyed by someone else's
        // output; retention is decay, `note_scroll` translation and reset.
        //
        // The unpaid press is a licence too (Rainbow Kitty only): a debounced
        // app repaints when it likes, often after the key stamps expire.
        // `move_licensed` is a public lockstep contract with the classic trail
        // (`app_render.rs`), so the extension lives at this seam, on the echo
        // shape. Program output banks no credits, so keyless caret walks still
        // decline.
        //
        // The park ([`HeldPark`]): a same-row backward move a stamp or the
        // in-flight pool stands behind is held for its return, in both
        // branches, before anything is consumed or scored; it is scored when
        // judged (flush or return).
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
        // A Backspace still owed licenses its own retreat and its erased keys'
        // late echo ([`Self::erase_hop`]).
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
                // A refused same-row forward hop is remembered for one delivery
                // receipt still in flight ([`PendingHop`]). The pool is kept:
                // it is empty, or one credit against a wider hop, which belongs
                // to the insert receipt and the next key's ledger bridge.
                self.remember_refused_hop(cr, pc, cc, now, cfg);
            } else {
                if park {
                    // The stalled key's rewrite, held so the return can spend
                    // the pool; with no return the flush refuses and forgets.
                    self.hold_park((pr, pc), (cr, cc), now, cfg, geom);
                    return false;
                }
                // A keyless backward or cross-row hop is not the echo shape
                // (ctrl+c clear, modal repaint, pager exit, Shift+Enter box
                // growth): the presses in flight are forgotten with it.
                self.forget_typed_credits(now);
            }
            self.log_decline(now, (pr, pc), (cr, cc), Self::DECLINE_NO_FRESH_HINT);
            return false;
        }
        if park {
            self.hold_park((pr, pc), (cr, cc), now, cfg, geom);
            return true;
        }
        // Direct-drive seam (tests call `spawn` without a tick): sparks and
        // thermals are wiped state, so the latch must not survive a spawn.
        self.unsettle();
        let licensed_row_before = self.last_licensed_row;
        self.last_licensed_row = Some((cr, now));
        self.insert.pending_hop = None;
        // Read before the classifier spends it: `glyph_echo` needs to know
        // whether a press was unpaid on arrival.
        let unpaid_before = self.typed_credits_within(now) >= 1;
        let exact_credits = exact_run.map(|_| self.type_press_ring);
        let mv = self.classify_move(pr, pc, cr, cc, now, cfg, geom, lane, call, exact_run);
        // A typed re-anchor landing on a glyph no live press typed
        // ([`MoveCtx::landing_foreign`]: zsh's Ctrl-R walking the caret along
        // the match) is not the keys' echo. Declined here, before thermals,
        // abandonment, licence takes and v2's `Move` (whose replay would lay
        // the refused cell). The classifier already forgot the presses and
        // spent the stamp; the caller treats it as declined.
        // A visible caret previously parked away from its output is not the
        // input row's witness. A torn search redisplay can print the live
        // key's exact glyph there before continuing on its real echo row.
        // Refuse acquiring the hand here; elapsed time adds no provenance.
        // Established typing and the anchored lane retain their own evidence.
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
        // The resize licence is consumed once per spawn for every style: the
        // first licensed move after a settled resize is the relayout
        // relocation, and it gets the beam only.
        let gap = self.update_typing_thermals(&mv);
        self.retire_abandoned_light(&mv);
        let reflow_licensed =
            take_hint_fresh(&mut self.reflow_hint, now, Self::REFLOW_HINT_FRESH).is_some();
        // Consume Return once: a fresh stamp is the bare-Enter licence for the
        // jump arm, and a stale one must not linger as morphology. Under
        // Rainbow Kitty a glyph's echo (a same-row forward typed hop with a
        // press in flight) never takes it, or the move would class `Return` and
        // forget the next key's credit; the Return stays for its own row
        // change (§17.3). Classic styles keep the class-blind take, in lockstep
        // with their trail twin.
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
        // The composer newline is consumed the same way, once; peeked but never
        // taken, one Shift+Enter would license every move for 250 ms.
        let newline_taken = if glyph_echo {
            None
        } else {
            take_hint_fresh(&mut self.newline_hint, now, Self::RETURN_HINT_FRESH)
        };
        let newline_licensed = newline_taken.is_some();
        // Consume the Tab / scripted gesture once, though the timestamp cannot
        // prove which later PTY move the child authored (class-blind: a typed
        // echo takes it too). A Tab's insert stamp armed at the same instant is
        // spent with it only when the gesture class licensed the move.
        let gesture_taken = self.user_gesture_hint.take();
        self.cue_move_sound(&mv);
        // The kill's retreat, one per kill: licensed by the retreat it is owed
        // ([`Self::kill_retreat_pending`], bounded by its own stamp, since
        // `kill_hint` is also the poof's row proof and a probe fence may clear
        // it), and spent here for every style. `note_motion` supersedes it.
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
        // A licensed move ends the last typed lay's wait for the caret's
        // rest ([`TornLay`]). A typed lay judges it first at its own origin,
        // where the last redisplay left the caret — before this key puts the
        // hand on its row, so a recall's hand is not this key's; any other
        // licensed move is a key the caret followed, and keeps it.
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
        // A licensed typed echo (paired with a fresh stamp or paid by the
        // in-flight pool) puts the hand on its landing row ([`Self::hand_row`])
        // until it presses anything else.
        let hand_before = self.hand_row;
        let unowned_before = self.unowned_caret;
        if mv.typed_hinted || mv.in_flight_licence {
            self.hand_lands_on(cr);
        }
        // Seam point 1 (§17.2, D14): the licensed move goes to v2 after the
        // licence gate, `classify_move`, thermals and one-shot takes, and v1
        // lays nothing for it, or two ribbons would draw.
        if self.v2.engaged() {
            self.hand_v2_the_move(&mv, licence, kill_retreat_at, call);
            // …and waits for the caret's rest itself.
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
        // Advance each style's ancillary rolling hue clock. Coalesced runs take
        // one step per swept cell so spawn metadata is cadence-independent.
        // Rainbow Kitty's visible ribbon and streak colours do not read it.
        let hue_step = match cfg.style {
            GlowStyle::Phaser => RAINBOW_PHASER_HUE_STEP,
            _ => RAINBOW_ROLL_HUE_STEP,
        };
        // Fade as one: under a live chain (Phaser / Beam) every typing spark's
        // death is re-priced to the last key's cadence, so a run fades together
        // instead of tail-first.
        if mv.typing
            && matches!(cfg.style, GlowStyle::Phaser | GlowStyle::Beam)
            && gap <= Self::PHASER_CHAIN_GAP_MAX
        {
            for s in self.sparks.iter_mut().filter(|s| s.typing) {
                let age = now.saturating_duration_since(s.born).as_secs_f32();
                s.life = s.life.min(age + spark_life);
            }
        }
        // Fire meteor for a licensed non-typing move: a row change or long
        // same-row skip flies one pixel-space streak along the true vector and
        // spawns no per-cell sparks.
        let fire_meteor =
            mv.fire && !mv.typing && (mv.dr_abs >= 1 || mv.dc_abs >= Self::METEOR_MIN_COLS);
        // Diagnosis census before and after the emitters, so `ctl trail` can
        // tell whether a licensed move put anything on glass. Spawn edge only.
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
        // Phaser's step is 2× so the fat beam sweeps the spectrum; a coalesced
        // run advances one step per laid cell (`hue_advances`).
        self.hue = (self.hue + hue_step * hue_advances).fract();
        self.spawn_lightning(&mv);
        self.spawn_landing_ring(&mv, fire_meteor);
        self.spawn_burst_particles(&mv, fire_meteor);
        if self.particles.len() > Self::MAX_PARTICLES {
            let drop = self.particles.len() - Self::MAX_PARTICLES;
            self.particles.drain(0..drop);
        }
        // Licensed and laid: score it for `trail status` ([`Self::spawns`]).
        // Spawn edge only; an idle cursor pays nothing.
        self.spawns += 1;
        if self.born_geometry_census() == born_before {
            // Licensed and classified, but the style's shape gates minted
            // nothing (a landing-only re-anchor, a declined zoom). The ring
            // names which dark cause it was.
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

    /// Record a licensed typed-class verdict: `licence=key` for a press-hint
    /// class, `licence=inflight` for a move the in-flight pool alone licensed,
    /// which is also tallied for `trail status`.
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

    /// How much born geometry the engine holds now: the census the spawn seam
    /// brackets its emitters with, so the diagnosis ring can tell a licensed
    /// move that painted from one that fell through every shape gate.
    #[inline]
    pub(super) fn born_geometry_census(&self) -> usize {
        self.sparks.len()
            + self.particles.len()
            + self.fire_meteors.len()
            + self.bolts.len()
            + usize::from(self.ring.is_some())
    }

    /// Classify one licensed cursor move (typing vs. jump, with the wrap,
    /// re-anchor and coalesced-echo collapses and bounded classifier hints)
    /// and feed the fast-glide velocity spine. The licence upstream already
    /// established a human pressed a key; this decides which choreography it
    /// earns.
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
        // A flushed park whose source row the content witness carried away
        // (`HeldPark::content_followed`): the proof a short wrap retreat needs.
        let content_followed = matches!(
            call,
            SpawnCall::FlushedPark {
                content_followed: true,
                ..
            }
        );
        // A single-cell advance is typing; a multi-cell delta is a real cursor jump.
        // The jump distance drives both the comet lifetime and the ring/particle burst.
        let dr_abs = (cr as i32 - pr as i32).abs();
        let dc_abs = (cc as i32 - pc as i32).abs();
        let raw_dist = dr_abs.max(dc_abs) as f32;
        // A same-row forward hop — the one shape a key's echo takes.
        let fwd = cr == pr && cc > pc;
        // What the move is to a Backspace still owed ([`Self::erase_hop`]): the
        // late echo of the keys it erased is paid by no press.
        let erase_hop = self.erase_hop((pr, pc), (cr, cc), now, cfg);
        let late_erased = match erase_hop {
            Some(EraseHop::Late(cells)) => usize::from(cells),
            _ => 0,
        };
        // A typing wrap (the cursor spilling from the last column to the next
        // row's start because a glyph filled the line) is continued typing, not
        // a jump; otherwise every wrap would fire a meteor across the line.
        // Detected by shape (down one row, landing at the left edge, from at or
        // near the last column) and its distance collapsed to one cell.
        //
        // The second arm catches the coalesced wrap echo (2-3 glyphs across the
        // fold in one observed move, landing at col 2-3 from within four
        // columns of the edge). Guards: `cc >= 1` (a plain Enter to col 0 only
        // matches the tight arm), a live typing rhythm (`last_type` within the
        // chain window, so a cold jump of the same shape keeps its drama), and
        // the main screen (on the alt screen the blink discriminator decides).
        let wrap_echo_rhythm = self.last_type.is_some_and(|t| {
            now.saturating_duration_since(t).as_secs_f32() <= Self::PHASER_CHAIN_GAP_MAX
        });
        let (pane_col0, pane_cols) = self.pane_span(geom);
        // The licence is decided before the fold shapes, since the coalesced
        // fold's second witness is the in-flight licence. Peeked: whether the
        // stamp is spent depends on whose echo the shape says this is
        // (`older_batch` below).
        let fresh_stamp = self.type_hint.peek_fresh(now, Self::TYPE_HINT_FRESH);
        let mut typed_at = fresh_stamp;
        let mut in_flight_licence = false;
        // An unpaid press is itself a typed licence. A debounced app may repaint
        // after every key stamp expired, which would leave the last cells dark
        // at the caret. With no fresh stamp, ask the ledger whether a press's
        // glyph is still unlaid. Self-limiting: program output banks no
        // credits (`a_caret_that_advances_with_no_unpaid_press_still_buys_nothing`).
        // Gated to the echo shape (a same-row forward advance or a paid fold);
        // other shapes keep the stamp window. The licence's clock is the oldest
        // unpaid press, matching `take_fresh`'s order and the born-at-key rule.
        if let Some(run) = exact_typed_run {
            typed_at = self.type_press_ring.slots[run.slot].map(|(at, _, _)| at);
            in_flight_licence = fresh_stamp.is_none();
        } else if typed_at.is_none() && self.unpaid_typed_echo(pr, pc, cr, cc, now, cfg, geom) {
            typed_at = self.type_press_ring.oldest_unpaid(now);
            in_flight_licence = typed_at.is_some();
        }
        // The tight arm is [`Self::fold_shape`] and the coalesced arm (2-3
        // glyphs across the fold in one move) [`Self::coalesced_fold_shape`],
        // shared with the in-flight seam. The coalesced arm also needs a live
        // typing rhythm, so a cold jump of its shape keeps its drama, or the
        // in-flight licence (the pool paid exactly; a stall ages the rhythm out).
        let tight_fold = self.fold_shape(pr, pc, cr, cc, geom);
        let coalesced_fold = self
            .coalesced_fold_shape(pr, pc, cr, cc, now, geom)
            .filter(|_| wrap_echo_rhythm || in_flight_licence);
        let shape_wrap = tight_fold.is_some() || coalesced_fold.is_some();
        // The cells a fold lays (origin tail plus landing head, zero tail after
        // a paid pending wrap, `fold_origin_tail`) are what it spends, so the
        // batch's remainder survives for the next row's keys.
        let fold_cells = tight_fold.or(coalesced_fold);
        // Typed re-anchor: a fresh typed or Backspace classifier collapses a
        // one-row move beyond the typed advance into TUI repaint morphology;
        // the caret never travelled the interpolated cells, so they get no
        // meteor, zoom, bolt or sweep. `dr_abs <= 1` covers wrap-down,
        // wrap-back-up and the box-growth wrap (`dr == 0` with a large column
        // delta); multi-row typed-paired moves (vim gg/G) keep the meteor.
        // `raw_dist > 2.0` leaves unproved hide-bridged moves alone; only a
        // held park whose content followed may use the short arm.
        let typed_pair = typed_at.is_some();
        // The Backspace's licence: its fresh hint, or the retreat the erases
        // still owe however late their echo.
        let owed_retreat = matches!(erase_hop, Some(EraseHop::Retreat(_)));
        let bs_pair = hint_fresh(self.quench_hint, now, Self::QUENCH_HINT_FRESH) || owed_retreat;
        // A Backspace's echo is a retreat and never explains a same-row forward
        // hop. The fresh quench hint vetoes the typed arms for backward and
        // cross-row shapes, but a forward hop in its window is the survivors'
        // echo (`abc⌫` draining as `+2`), so the veto lifts for it.
        let bs_fwd = bs_pair && !fwd;
        // A Return's late response beside a type-ahead glyph's fresh stamp is
        // the Return's move: it neither re-anchors on the stamp nor pops it.
        let return_paired = !fwd && hint_fresh(self.return_hint, now, Self::RETURN_HINT_FRESH);
        // The composer newline's row change is paired the same way. Unlike the
        // Return, the chord banked its own typed stamp and the break is its
        // echo: the take pops the oldest fresh stamp (the chord's) and leaves
        // the glyph's.
        let newline_paired = !fwd && hint_fresh(self.newline_hint, now, Self::RETURN_HINT_FRESH);
        // Alt-screen blink classifier: the repaint blink separates a full-redraw
        // TUI re-anchor from a deliberate vim/less jump; vacuous on the main
        // screen.
        let blink_fresh = self.blink_fresh(now);
        // A fresh nav stamp vetoes a dying typed re-anchor: Ctrl-A/E and
        // Home/End are deliberate jumps with their own meteor (peeked; the nav
        // classifier consumes). Under Rainbow Kitty a same-row forward hop the
        // in-flight pool pays for is a glyph's echo landing after the arrow's
        // press, so the hint is left for the arrow's own hop. A forward hop in a
        // moving kill's window is the glyphs it will erase landing late: it flies
        // as `Synthetic`, and the retreat keeps the nav stamp, `Kill` and drain.
        let glyph_fwd = matches!(cfg.style, GlowStyle::RainbowKitty)
            && fwd
            && (hint_fresh(self.kill_retreat_pending, now, Self::KILL_HINT_FRESH)
                || (dc_abs == 1 && self.typed_credits_within(now) >= 1)
                || (dc_abs >= 2 && self.typed_share_paid(cr, pc, cc, now)));
        let nav_paired = !glyph_fwd && hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH);
        // Echo run (not Rainbow Kitty, whose `rainbow_coalesce` owns it): a
        // fast burst's echo advancing up to `HIDE_BRIDGE_TYPED_MAX_DIST` cells
        // between frames under a fresh typed classifier is continued typing,
        // laid as per-key typing sparks, not a jump or a landing-only re-anchor.
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
        // The typed key's move under Rainbow Kitty: a typed-paired move no
        // Backspace or nav one-shot owns; and the typed forward hop, its
        // same-row forward shape (on the alt screen, only under the blink
        // discriminator). Every typed arm below is one of these plus a width
        // clause.
        let typed_key =
            matches!(cfg.style, GlowStyle::RainbowKitty) && typed_pair && !bs_fwd && !nav_paired;
        let typed_fwd = typed_key && fwd && (!self.ctx_alt || blink_fresh);
        // Rainbow Kitty typed coalesce: a same-row forward hop of
        // 2..=`RAINBOW_TYPED_SWEEP_MAX` cells is continued typing observed late
        // (PTY batching, frame quantization), not a jump, when the presses that
        // typed it pay under the share rule ([`Self::typed_share_paid`]); vim's
        // `w` is one credit echoing as a multi-cell hop. It collapses `dist`
        // like a wrap, so its swept cells join the ribbon.
        let rainbow_coalesce = typed_fwd
            && dc_abs >= 2
            && dc_abs as usize <= Self::RAINBOW_TYPED_SWEEP_MAX
            && self.typed_share_paid(cr, pc, cc, now);
        // The one shape only the credit budget refused: everything else said
        // coalesced typing. Recorded (`no-credits`) so `ctl trail` can tell a
        // budget refusal from a non-typing shape.
        let credit_starved = typed_fwd
            && !rainbow_coalesce
            && dc_abs >= 2
            && dc_abs as usize <= Self::RAINBOW_TYPED_SWEEP_MAX;
        // The same shape past the cap: a landing-only re-anchor and a forget
        // edge, since the presses cannot describe a hop the cap refuses.
        let typed_over_cap = typed_fwd && dc_abs as usize > Self::RAINBOW_TYPED_SWEEP_MAX;
        // The batch behind the stamp: a coalesce wholly covered by presses
        // older than the fresh stamp is those presses' batch, so the sweep is
        // dated at the oldest unpaid press and the stamp stays banked for its
        // own echo a frame later.
        let older_batch = rainbow_coalesce
            && fresh_stamp.is_some_and(|stamp| {
                self.type_press_ring.cells_where(now, |t| t < stamp) >= dc_abs as usize
            });
        let typed_at = if older_batch {
            self.type_press_ring.oldest_unpaid(now).or(typed_at)
        } else {
            typed_at
        };
        // A credit is spent by the cells it lays, for every forward same-row
        // typed echo and every fold: a press whose glyph has landed must not
        // buy a second cell elsewhere, so what remains in the ring is exactly
        // light paid for and not yet delivered.
        let lays_typed_cells =
            typed_key && ((fwd && (rainbow_coalesce || dc_abs == 1)) || shape_wrap);
        // A typed re-anchor (the box-growth wrap, a flushed park, a fresh-stamp
        // `w`) consumes the stamp and lays exactly one cell, the landing;
        // unspent, its credit would license the next keyless +1 as `inflight`.
        // It lays only while a press is unpaid and on a key's stamp, never on
        // the in-flight pool alone, except a delayed composer fold whose old tail
        // visibly moved after the key's glyph (`key_pushed_text_down`).
        let re_anchor_landing = typed_key
            && exact_typed_run.is_none()
            && (!in_flight_licence || self.carried_key_move == Some(((pr, pc), (cr, cc))))
            && re_anchor
            && !lays_typed_cells
            && self.typed_credits_within(now) >= 1;
        // The landing is the key's own glyph. The landing law is for hops whose
        // landing cell is the typed glyph (box-growth wrap, flushed park, a
        // refused coalesce). zle's incremental search is the counter-shape: it
        // echoes keys on the status row below and walks the visible caret along
        // the match row, one credit per hop, so laying landings would draw a
        // band on history text. On the visible lane (the anchored lane has its
        // own gate in [`Self::echo_anchor_pass`]) the landing is laid only
        // where this frame's caret-row probe ([`Self::probe_glyph_before`])
        // holds a live press's own glyph. A foreign landing forgets the presses
        // and [`Self::spawn_judged`] declines the move `program-row`. Unknown
        // (no probe this frame, a glyph-less press, an empty pool) keeps the
        // landing law.
        //
        // The witness reads the cell the re-anchor lays. Two composer verdicts
        // move that cell off `(cr, cc − 1)` and are read first:
        // * a soft-wrapped caret ([`Self::soft_wrapped_caret`]) lays its one
        //   cell at the origin `(pr, pc)`, where the key's glyph stands, so the
        //   witness is the origin's glyph in the probed row above (a glyph no
        //   press typed is still refused);
        // * a lifted word ([`Self::lifted_word`]) lays no landing, so the gate
        //   stands down with the sweep and the move reaches v2 with its `Lift`;
        //   zle's hops never rewrite the row above.
        // The typed fold never reaches the gate; a key that pushed text down
        // ([`Self::key_pushed_text_down`]) and a reflowed soft-wrapped word
        // ([`Self::carried_word_left`]) land right of the key's glyph and pass.
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
        // A flushed park is witnessed by the glyph it was held over
        // ([`HeldPark::landing_cell`]), read against the presses live at the
        // park's clock; a blank landing lays nothing, so Ink's park beside the
        // prompt keeps its verdict and presses.
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
        // A refused re-anchor lays nothing, so it carries no soft-wrap verdict
        // (the seam declines it before v2 sees a `Move`).
        let soft_wrap = soft_wrap.filter(|_| !landing_foreign);
        let mut fold_laid = 0;
        if let Some(run) = exact_typed_run {
            self.type_press_ring.retire_before_slot(run.slot);
            let _ = self.type_press_ring.spend_counting(now, run.cells);
        } else if lays_typed_cells || re_anchor_lays_landing {
            // A soft-wrapped caret's fold or re-anchor lays exactly one cell,
            // the key's glyph at the origin ([`Self::soft_wrapped_caret`]);
            // spending the fold's full count would spend presses still to come.
            let credits = self.typed_credits_within(now);
            // A late erased echo's cells are paid by no press; the rest are the
            // presses typed before the erase, the oldest.
            let laid = if re_anchor_lays_landing || soft_wrap.is_some() {
                1
            } else {
                fold_cells.unwrap_or(dc_abs as usize - late_erased)
            };
            let cells = laid.min(credits);
            if fold_cells.is_some() {
                fold_laid = cells;
            }
            // Spent by the cells it lays, oldest-first, so one pool of typing
            // never funds a second multi-cell echo.
            let _ = self.type_press_ring.spend_counting(now, cells);
        }
        // Consume the typed classifier once (one hint, one echo), unless this
        // hop was the older presses' batch and the stamp's own press is still
        // unpaid, and never on a Return's response or a Backspace's retreat.
        if let Some(stamp) = fresh_stamp
            && !(return_paired || bs_fwd)
        {
            let own_press_unpaid =
                older_batch && self.type_press_ring.cells_where(now, |t| t >= stamp) >= 1;
            if !own_press_unpaid {
                let _ = self.type_hint.take_fresh(now, Self::TYPE_HINT_FRESH);
            }
        }
        // `rainbow_coalesce` (Rainbow Kitty) and `echo_run` (other styles) are
        // exclusive by style; with `wrap` they classify the move as typing.
        let dist = if wrap || rainbow_coalesce || echo_run {
            1.0
        } else {
            raw_dist
        };
        let typing = dist <= 1.0;
        let fire = matches!(cfg.style, GlowStyle::Fire);
        // Momentum direction: only a one-column forward advance on the same row
        // earns heat and coal; a deletion echo earns nothing.
        let forward = typing && cr == pr && cc as i32 == pc as i32 + 1;
        // A same-row forward hop is never the deletion (see `bs_fwd`), but the
        // quench hint is consumed by it anyway: the erase is already in the
        // drained row, and a standing one-shot would license every keyless
        // `+1` until it expired. One hint, one echo, whatever the shape,
        // including a jump; `typing` gates only the deletion class. The one
        // forward hop that leaves it is the late echo of a key the Backspace
        // erased before that key echoed: the erase's retreat is still to come.
        // The owed retreat itself spends it, however late.
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
                // Every retired key's late echo precedes any retreat.
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
        // Navigation: a move paired with a fresh nav hint is scrubbing, not
        // writing, and earns no heat or arrival flare. Consumed on a paired move
        // in any direction (a `cr == pr` conjunct would classify held Up/Down
        // as typing and max the heat).
        let nav_taken = if nav_paired {
            take_hint_fresh(&mut self.nav_hint, now, Self::NAV_HINT_FRESH)
        } else {
            None
        };
        let navigation = nav_taken.is_some();
        // Typing momentum builds in [`Self::update_typing_thermals`] on the
        // typed-paired half only; rate normalization lives in
        // [`TypingMomentum::advance`], so key count never buys momentum.
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
