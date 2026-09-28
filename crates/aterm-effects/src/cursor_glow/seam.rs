// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SEAM — judging each observed caret move: licences, the park, the
//! hidden-caret bridge and the classification that decides what light a
//! move may mint.

use super::*;

/// Which lane is asking [`CursorGlow::spawn`] to judge a move. The
/// delivered-insert arm reads it for the ROW IDENTITY it cannot derive
/// itself: the visible lane's caret is its own witness (the DEC cursor sits
/// where the hand is), while the anchored lane's print anchor is shared by
/// every row a TUI repaints and carries the lane's own verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SpawnLane {
    /// The visible-caret move lane (`tick`): the DEC cursor moved.
    Visible,
    /// The hidden/parked-caret echo lane (`echo_anchor_pass`): the print
    /// anchor advanced; `insert_ok` is that lane's identity verdict for the
    /// delivered insert on this row.
    Anchored { insert_ok: bool },
}

/// How a held park was RELEASED by the move that reached the seam
/// ([`CursorGlow::release_held_park`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ParkRelease {
    /// The return: the move is judged as `(pr, pc) -> target` — the park's
    /// row and origin on the visible lane (the park's `Move` was withheld,
    /// so the mirror still sits there), the move's own source on the
    /// anchored lane.
    Return { pr: u16, pc: u16 },
    /// The same-end rewrite: the park is dropped and nothing is judged.
    Cancelled,
}

/// What [`CursorGlow::spawn_judged`] is judging: a move observed live, or a
/// held park flushed at its own clock ([`CursorGlow::flush_park`]). A live
/// move on the visible lane may be HELD as a park; a flushed park never is,
/// and its re-anchor's landing sweep is withheld when no glyph sat at its
/// landing cell when it was held ([`HeldPark::landing_glyph`]) — the credit
/// spend, the stamp pop, the ring row and the refused late return are
/// untouched — and the landing gate witnesses the GLYPH that sat there
/// ([`HeldPark::landing_cell`]).
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
    /// The seam's HOLDS EXPIRE at the tick, before this tick's move: a held
    /// park past its window is judged now — a park followed by silence still
    /// lands its `Move`, so the engine's mirror is honest ([`HeldPark`]) —
    /// and the presses an unknown insert's hop echoed are bounded away once
    /// the hop is old enough for any next-frame echo of theirs to have spent
    /// them ([`InsertSeam::retire_orphans`]).
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
            // The generic `+1` path must NEVER read these keys: an ambient
            // different glyph could spend one. Restore only the one whose
            // exact row transition is proved, for that one spawn call.
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

    /// THE SOURCE OF A MOVE WHOSE CARET WAS HIDDEN LAST FRAME — a pure read
    /// with four lanes, first answer wins: a proved foreign-row park's own
    /// return (its landing); the estimate's witness (a caret shown exactly
    /// where it was last OBSERVED, left of the anchored lane's relocation
    /// estimate on its row, never moved — the estimate was wrong, and this
    /// is a same-cell completed boundary: nothing judged, held, spent or
    /// forgotten); then the last visible cell, when the landing is
    /// plausible from it — the ConPTY hide bridge (conhost hides the cursor
    /// ~20-35 ms on every keystroke echo; a landing inside
    /// `HIDE_BRIDGE_MS` within the typed or nav reach), a fresh NAV witness
    /// (a word or line move's whole sound is echo-born; the reach is the
    /// focused pane's width and a wrapped box's height, the source and the
    /// landing bounded on both sides of the press by `nav_bridge_admits`),
    /// or the in-flight echo (a same-row forward reappearance the unpaid
    /// presses pay for, whatever the hide's age: keys were typed since the
    /// caret was last seen, and `spawn` judges the hop under the full share
    /// rule), or the delivered insert's echo (a same-row forward
    /// reappearance a fresh Tab or paste licence lays, whatever the hide's
    /// age: [`Self::visible_insert_echo`], 2026-09-22). `None`: a declined
    /// relocation. Selects geometry only — the licence gate still decides
    /// admission.
    pub(super) fn hidden_bridge_source(
        &self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> Option<(u16, u16)> {
        // A hidden repaint can return from a proved foreign-row park
        // outside the generic hide bridge's geometric reach. Only the
        // original input row and its paid forward echo claim custody.
        if let (Some(p), Some((row, col))) = (self.held_park, cur)
            && p.cross_row
            && self.park_source_confirmed
            && p.fresh(now)
            && row == p.row
            && col >= p.origin
            && (col == p.origin || self.park_return_paid(&p, col, now))
        {
            return Some((p.landing_row, p.landing));
        }
        // THE ESTIMATE'S WITNESS: `last_visible` may be the anchored lane's
        // RELOCATION — where it estimated a hidden caret would be shown — and a
        // caret shown exactly where it was last OBSERVED, left of that
        // estimate on its row, never moved: the estimate was wrong (an
        // advance at the row's end that was not the caret's). A
        // same-cell completed boundary, then — nothing judged, held,
        // spent or forgotten — where the bridge below would have
        // refused a backward hop or, past its reach, declined a
        // relocation and wiped the pool.
        if let (Some(shown), Some(((er, ec), _)), Some((cr, cc))) =
            (self.hide_bridge_shown, self.last_visible, cur)
            && (cr, cc) == shown
            && cr == er
            && cc < ec
        {
            return Some((cr, cc));
        }
        let ((r, c), seen) = self.last_visible?;
        let fresh = now.saturating_duration_since(seen).as_millis() as u64
            <= crate::cursor_trail::HIDE_BRIDGE_MS;
        // A fresh typed classifier widens the plausible bridge reach (PEEKED
        // — `spawn` still owns candidate consumption): batched echoes hop farther than 2 cells
        // inside one hide window, and dropping them left the trail with a
        // hole on every fast burst. TYPE hint ONLY — the trail engine's
        // twin has no quench hint and their clear_typed semantics differ,
        // so keying on quench here would let the two engines disagree on
        // the same bridged move (the lockstep contract). This only selects
        // geometry; the universal candidate gate still decides admission.
        let typed_fresh = self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH);
        // A fresh NAVIGATION hint outranks the hide window entirely
        // (the silent word hop; lockstep with `CursorTrail::tick`).
        // Option+Left/Right, Ctrl+arrow and Alt-b/f
        // have NO key-time cue — the keyed seam opens only for a typed
        // glyph (D11 vetoes a nav pre-cue: a no-op Ctrl-E must stay
        // silent) — so a word move's whole sound is echo-born, and a
        // landing this bridge refused died with no cue, no light and no
        // ring row. A key WAS pressed; this landing is its echo across a
        // word or a line — Option+Left crosses a word, `Home` a line — so
        // an 8-cell cap fitted to batched TYPING is a cap on the gesture.
        //
        // GESTURE-SHAPED, NEVER GRID-SHAPED: the nav reach is the FOCUSED
        // PANE's width across and a wrapped input box's height down — an
        // unbounded reach makes `plausible` vacuous and hands the program's
        // own caret relocation the key's licence, the full ribbon and the
        // meteor voice on a key that moved nothing.
        //
        // NOT a widening of the anti-stray law. `spawn` still owns the
        // licence and consumes the hint exactly once; a STALE hint bridges
        // nothing, so an app repositioning its caret mid-repaint with no
        // key behind it keeps the pinned 2-cell refusal; and the SOURCE is
        // bounded on BOTH sides of the press by `nav_bridge_source_ok` —
        // too old to be the cell this key moved from (a viewport parked in
        // history), or seen still at rest so long AFTER the press that the
        // press provably moved nothing.
        //
        // AND THE LANDING IS BOUNDED: a
        // hidden→visible landing later than `NAV_BRIDGE_LANDING_MS` after
        // the press is not that press's echo, however fresh the hint —
        // the hint lives 250 ms so a second press gets its own licence,
        // the bridge is a narrower claim. `nav_bridge_admits` is the one
        // decision both engines call. And a landing two or more rows off
        // must be Home's shape (`BridgeReach::line_start`): what no bound
        // on time can separate — a program re-laying its input box a few
        // rows down within the echo window — the shape refuses.
        let nav_bridge = hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            && self
                .nav_hint
                .is_some_and(|key| crate::cursor_trail::nav_bridge_admits(key, seen, now));
        let reach =
            crate::cursor_trail::hide_bridge_reach(typed_fresh, nav_bridge, self.pane_columns);
        let plausible = cur.is_some_and(|to| reach.admits((r, c), to));
        // THE STALLED BATCH'S REAPPEARANCE (Rainbow Kitty only): a frame
        // catches the merged repaint's bracket hidden and the next sees the
        // caret thirty cells on — past any bridge reach, a DECLINED
        // relocation that would wipe every bank.
        // A same-row FORWARD reappearance the in-flight presses pay for
        // is the echo shape (`unpaid_typed_echo`: the unpaid presses
        // pay for it, this style only), whatever the hide's age: the
        // presses say keys were typed since the caret was last seen,
        // and `spawn` judges the hop under the full share rule.
        let in_flight_echo =
            cur.is_some_and(|(cr, cc)| self.unpaid_typed_echo(r, c, cr, cc, now, cfg, geom));
        // THE LICENSED INSERT'S REAPPEARANCE (2026-09-22, Rainbow Kitty
        // only — the one style with a delivered-insert class). A Tab
        // completion or a paste whose echo frame the host presented TORN —
        // the caret hidden and nothing yet written (a 1024-byte PTY read
        // ending at `?25l`, the `?2026` hold capped) — reappears the insert's
        // width on with no print the anchored lane could have judged under
        // the hidden caret: it was refused here as a `hidden-relocation`
        // (the Tab) or silently (the paste, whose gesture the enqueue
        // revoked), the licence lapsed unspent, the three cells of `INTO `'s
        // `TO ` were laid by nobody, and the hand's next key past them
        // minted a second band with the dark gap between
        // (`tests/licensed_insert_band.rs`). The insert's own witness is its
        // delivery, not the hide window, so the reach is the licence's: a
        // same-row forward reappearance `spawn` would lay as that insert on
        // the visible lane — exactly the predicate it judges with
        // ([`Self::visible_insert_echo`]), so the bridge admits nothing the
        // insert arm then refuses. An insert already laid under the hidden
        // caret (its print on the torn present) was spent there and admits
        // nothing more.
        let insert_echo =
            cur.is_some_and(|(cr, cc)| self.visible_insert_echo(r, c, cr, cc, now, cfg).is_some());
        // The NAV witness is an alternative to `fresh` (a stamp the key
        // just laid), not to `plausible` (the reach the same nav witness
        // already widened above); the in-flight echo and the insert's echo
        // remain their own, reach-free lanes.
        (((fresh || nav_bridge) && plausible) || in_flight_echo || insert_echo).then_some((r, c))
    }

    /// JUDGE THE OBSERVED CARET against its source: a real move between two
    /// visible positions (`spawn_from`, the hide bridge included) is judged
    /// by `spawn` on the visible lane — the crown window sized to its
    /// distance (a single-cell typing advance gets the longer window so the
    /// crown chains across human inter-key gaps); an unlicensed move mints
    /// nothing and clears nothing, advances the move clock, drops the
    /// cursor-relative crown and tells v2's mirror where the caret is. A
    /// hidden→visible landing the bridge refused is a declined relocation
    /// (logged when a key asked for it; the provenance retired); a
    /// reappearance on the cell the caret was last seen at is a completed
    /// hidden boundary (stamps, credits, the insert and a held park spared
    /// while fresh); a first draw with no source seeds the anchor and
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
            self.crown_window_ms = if let Some(p) = cfg.pack.as_ref() {
                // Trail Pack: the crown's typing/jump windows are the pack's own
                // `crown.typing_window_ms`/`jump_window_ms` (Custom-only). Every
                // built-in (no pack) keeps the shared CROWN_* windows byte-for-byte.
                if dist <= 1 {
                    p.crown.typing_window_ms as u64
                } else {
                    p.crown.jump_window_ms as u64
                }
            } else if dist <= 1 {
                Self::CROWN_TYPING_MS
            } else if matches!(cfg.style, GlowStyle::Fire) {
                // The crown must survive the meteor's flight (≤320 ms) so the
                // ARRIVAL flare still has a live crown to erupt through.
                Self::CROWN_MS + 320
            } else {
                Self::CROWN_MS
            };
            self.coalesced_prefix_echo((pr, pc), (cr, cc), now, cfg, geom);
            // An unknown-width insert's one or two queued keys were removed
            // from the generic press pool. Restore only the exact observed
            // glyphs for this one spawn. A one-cell frame keeps the next key
            // escrowed; an exact two-cell frame spends both together. Every
            // mismatch or refused spawn drops the entire remainder.
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
            let admitted = self.spawn(pr, pc, cr, cc, now, cfg, geom, SpawnLane::Visible);
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
                // AN UNLICENSED MOVE MINTS NOTHING AND CLEARS NOTHING. The
                // move clock advances (this cursor really did move, so the
                // next gap measurement must start here), and the crown — which
                // is CURSOR-RELATIVE, so carrying it across a program warp
                // would teleport it — is dropped. Neither is resident light:
                // sparks, thermals and the wake stay exactly as the user's own
                // typing left them, and leave only by decay, `note_scroll`
                // translation, or `reset`.
                self.last_move = Some(now);
                self.crown_until = None;
                // …AND THE MIRROR FOLLOWS THE OBSERVED CARET ACROSS A ROW
                // (the abandoned band): v2's caret mirror is otherwise
                // written only by a licensed move, so after a declined
                // relocation — a TUI re-laying its input box a row up with
                // no key behind the move; the owner's live ring read
                // `declined no-fresh-hint (41,2)->(40,2)` — the next typed
                // key would lay its cell at the STALE mirror on the
                // abandoned row. This tells the engine where the caret is and mints
                // nothing: no licence, no meteor, no light, no clock
                // ([`rk::Engine::observe_caret`]); it takes effect after
                // this tick's ingest, so a key buffered before the
                // relocation still lays where it was typed. A SAME-ROW
                // unlicensed hop is left alone by the engine itself: that
                // hole is the echo ledger's to measure and pay.
                if self.v2.engaged() {
                    self.v2.observe_caret((cr, cc));
                }
            }
        } else if declined_hidden_relocation {
            // THE REFUSAL IS ON THE LEDGER WHEN A KEY ASKED FOR IT: a
            // relocation the bridge refuses never reaches `spawn`, so a
            // licensed press whose move died here would otherwise be
            // invisible to `aterm ctl trail` — licensed and declined both
            // unchanged, no row at all. A PROGRAM relocation (no fresh
            // licence term) stays unlogged.
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
            // A fresh/reset engine has no truthful source cell for this
            // landing. Seed the anchor but consume every classifier dark,
            // otherwise the next unrelated CUP can borrow a license that was
            // stamped before first draw.
            self.retire_all_movement_provenance();
            // v2's mirror is deliberately NOT seeded here: a fresh engine
            // lays nothing for a key struck before its first licensed move
            // (`Engine::reset`'s own law — the echo's move seeds the mirror
            // and the Sweep lays the glyph), and a mirror seeded at `(r, 0)`
            // would fold that key's neighbour cell onto the row above.
        }
    }

    /// The only witness that can release one or both NEXT keys held out of
    /// the generic pool after an unknown insert: exactly those consecutive
    /// cells, still on the insert's row, change from blank to their queued
    /// glyphs in a frame with a new PTY print generation. A two-cell hop
    /// needs BOTH exact cells in one current and one prior row probe; it may
    /// not claim a partial batch. A later frame must also retain the prefix
    /// already claimed. A different glyph, a moved/re-written insert, a
    /// newer pending key or an old print discards the remainder.
    /// The PTY cannot distinguish a delayed key from program output that
    /// exactly mimics its glyph, cell, caret, and print generation; this is
    /// the existing one-key proof applied once per key, never a generic
    /// licence for any unrelated program output.
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
        // the program has since rewritten. Both probes must still carry the
        // original glyph; otherwise the new key would start an isolated band.
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

    /// The caret was SHOWN at `cur` this tick (or not shown at all): the
    /// classifier's source follows it, and a shown caret closes the hide
    /// bridge's estimate witness.
    pub(super) fn track_shown_caret(&mut self, cur: Option<(u16, u16)>, now: Instant) {
        self.last = cur;
        if let Some(c) = cur {
            self.last_visible = Some((c, now));
            self.hide_bridge_shown = None;
        }
    }

    // ----- spawning -----

    /// THE LICENSE — *did a human touch the keyboard just now?*
    ///
    /// See `docs/design/EFFECTS-LICENSE-REDESIGN.md`. This is the whole
    /// question the effects engine is entitled to ask before it mints light,
    /// and it is answered by the key-hint stamps the host already arms at the
    /// input boundary, each under its own existing freshness constant (the
    /// 0.25 s class). It generalizes the one conjunct v0.43.0 already had in
    /// miniature — the momentum spine's `typed_pair`, "earned by real typing
    /// ONLY" — from momentum to the spawn seam.
    ///
    /// PEEKS, never consumes: `spawn`'s classifier owns hint consumption
    /// (one hint, one echo), and the license must not spend a stamp the
    /// classifier still needs to tell typing from a re-anchor.
    ///
    /// CLASS-BLIND on purpose. WHICH choreography fires stays
    /// [`Self::classify_move`]'s job; the license only says whether any may.
    ///
    /// Deliberately NOT license terms, though they are live stamps:
    /// * `reflow_hint` / `blink_hint` — a resize and a TUI's repaint blink are
    ///   not keyboard presses. They stay morphology inputs.
    /// * `kill_hint` — a kill that moves the caret (Ctrl-U/W, word-backspace)
    ///   arms `nav_hint` and is licensed by it; a stationary kill (Ctrl-K,
    ///   Alt-D, forward Delete) moves no caret, so `spawn` never runs for it
    ///   and its poof is minted from `tick`'s erase detector, not here.
    ///
    /// `synthetic_note_pending` from the design's predicate needs no field of
    /// its own: [`Self::note_synthetic_move`] stamps `user_gesture_hint` and
    /// [`Self::note_synthetic_typed`] stamps `type_hint` at the same instant,
    /// so a scripted preview is already licensed by the disjuncts above (see
    /// `a_synthetic_note_licenses_its_own_move`).
    #[must_use]
    pub fn move_licensed(&self, now: Instant) -> bool {
        self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH) || self.one_shot_licensed(now)
    }

    /// The ONE-SHOT classes of [`Self::move_licensed`] — every licence term
    /// but the typed stamp bank: a Backspace, a navigation key, Enter, a
    /// newline, a Tab / scripted gesture.
    pub(super) fn one_shot_licensed(&self, now: Instant) -> bool {
        hint_fresh(self.quench_hint, now, Self::QUENCH_HINT_FRESH)
            || hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            || hint_fresh(self.return_hint, now, Self::RETURN_HINT_FRESH)
            || hint_fresh(self.newline_hint, now, Self::RETURN_HINT_FRESH)
            || hint_fresh(self.user_gesture_hint, now, Self::USER_GESTURE_HINT_FRESH)
    }

    /// THE LICENCE AT THE SEAM ([`Self::spawn_judged`]'s gate):
    /// [`Self::move_licensed`] for every style — the classic trail's
    /// lockstep contract, untouched — except that under Rainbow Kitty a
    /// TYPED STAMP licenses light only while the press ring still OWES A
    /// CELL. The stamp bank's own law — "program output beyond what keys
    /// paid for still finds an empty queue" — does not hold once echoes
    /// coalesce: a three-key `+3` spends three presses but pops ONE stamp,
    /// the `credit_starved` / `typed_over_cap` forget edges forget K
    /// credits and pop one, a stamp re-banked at a paste's delivery sits
    /// after its own press — every such path leaves fresh stamps with
    /// nothing to pay for, on which a keyless program `+1` inside the
    /// quarter second would be laid as `key` with zero credits and a
    /// keyless wide hop would light its landing. The credit ring is the
    /// ledger that does hold, so it is consulted here, once, for every
    /// path that reaches the zero-credit / fresh-stamp state. The one-shot
    /// classes (a Backspace's retreat, an arrow, Enter, a Tab) carry no
    /// credit by design and license exactly as before.
    pub(super) fn seam_licensed(&self, now: Instant, cfg: &GlowConfig) -> bool {
        let stamp = self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH)
            && (!matches!(cfg.style, GlowStyle::RainbowKitty)
                || self.typed_credits_within(now) >= 1);
        stamp || self.one_shot_licensed(now)
    }

    /// This row's remembered print endpoint and brand, with its slot
    /// ([`Self::anchor_rows`], LRU-lite: a linear scan of four rows).
    pub(super) fn anchor_row(&self, row: u16) -> Option<(usize, AnchorRow)> {
        self.anchor_rows
            .iter()
            .enumerate()
            .find_map(|(i, entry)| entry.filter(|mem| mem.row == row).map(|mem| (i, mem)))
    }

    /// Write a row's print endpoint and brand back: in place when the row
    /// had a slot, else at the round-robin head (the oldest row is
    /// forgotten).
    pub(super) fn remember_anchor_row(&mut self, slot: Option<usize>, mem: AnchorRow) {
        match slot {
            Some(i) => self.anchor_rows[i] = Some(mem),
            None => {
                self.anchor_rows[self.anchor_rows_head] = Some(mem);
                self.anchor_rows_head = (self.anchor_rows_head + 1) % ANCHOR_ROWS;
            }
        }
    }

    /// THE HIDE-BRIDGE SOURCE FOLLOWS A LICENSED ANCHORED ECHO under a
    /// hidden caret: the echo is the trail's only witness of where the
    /// caret went and the engine mirror is already at its landing, so the
    /// source the reappearance is judged from moves to WHERE THE CARET WILL
    /// BE SHOWN — not the anchor itself. `ac` is one past the last printed
    /// glyph, the caret's show position only for end-of-line typing: the
    /// pending-wrap print at the pane's edge leaves `ac == pane_col1`,
    /// off-grid, while the DEC caret is shown parked on the last column
    /// (the source is clamped there, so the show is a same-cell boundary
    /// and the paid edge cell's witness survives it); mid-line typing (Ink
    /// re-lays the tail, so the anchor is the row's END while the caret is
    /// shown mid-row) advances the source from where the caret was last
    /// shown by the echo's width instead. Judged from a stale source the
    /// reappearance re-judged the whole hidden run as one hop — refused by
    /// the share rule, one press spent on the landing, the rest forgotten,
    /// the keys' real echo dark. The cell the caret was actually last seen
    /// at is kept beside the estimate ([`Self::hide_bridge_shown`]): a show
    /// landing exactly there, left of the estimate, is no move. Rainbow
    /// Kitty only, like the in-flight bridge the relocation exists for.
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

    /// The glyph the terminal holds at the LAST cell of the print run that
    /// ended at `(row, end)`, as THIS frame's host sampled it: the anchor's
    /// own glyph when the host handed it over with this very anchor
    /// ([`Self::observe_print_anchor_glyph`] — any row), else
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

    /// The glyph at `(row, end - 1)` in THIS frame's row probe — the caret's
    /// own row ([`Self::observe_row`]) or one of its two flanking rows
    /// ([`Self::observe_neighbor_rows`]), captured at `now`: the last cell of
    /// a print run ending at `(row, end)`, or the one cell a typed
    /// re-anchor would lay: its landing, left of a caret at `(row, end)`, or
    /// a soft-wrapped caret's ORIGIN on the row above (`end` one past it —
    /// [`Self::soft_wrapped_caret`]). `None` —
    /// unknown — for any other row, a probe from another frame (a held
    /// park judged at its own clock included), or a neighbour the host did
    /// not capture. A trimmed row is blank past its end (the probe's
    /// convention).
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

    /// A new glyph immediately before the observed caret, backed by the
    /// oldest eligible one-cell press and a resident hand-owned cell. A moving
    /// prefix needs the short key hint; the same-caret path may use the full
    /// in-flight patience because the exact blank-to-glyph row change is its
    /// only movement witness. The row probe and print generation were
    /// already sampled by the host; this reads them only on a candidate,
    /// never on an idle frame.
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
        let credits = self.type_press_ring;
        self.type_press_ring.retire_before_slot(slot);
        let admitted = self.spawn(pr, pc - 1, pr, pc, now, cfg, geom, SpawnLane::Visible);
        if !admitted {
            self.type_press_ring = credits;
        } else {
            self.remember_exact_first_cell(pr, pc - 1, now);
        }
    }

    /// A whole first composer batch can arrive after its `a` has aged beyond
    /// the 250 ms per-key hint, or after wrap-fill was refused and left no
    /// owned cell. Require every visible cell from the missing left edge
    /// through the observed caret to match consecutive unpaid exact keys,
    /// plus a fresh same-row probe and a newer PTY print generation. The
    /// ordinary `spawn` still judges/spends one credit for the missing `a`;
    /// the observed move spends the remaining keys unchanged.
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
        for (i, cell_col) in ((col - 1)..target).enumerate() {
            glyphs[i] = self
                .row_cur
                .get(usize::from(cell_col))
                .copied()
                .unwrap_or(' ');
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

    /// A TUI can redraw the visible caret back onto its old cell after a
    /// one-glyph echo. Codex does this for the first glyph on a wrapped
    /// composer row, then prints its status row in the same synchronized
    /// bracket: neither the observed caret nor the terminal's LAST print
    /// anchor can identify the new glyph. The already-captured caret-row
    /// probes can. Require an in-flight unpaid key with its exact glyph identity,
    /// a new PTY print generation, and that same glyph replacing a blank
    /// immediately behind the unchanged caret. An ambient Braille particle
    /// cannot spend the pending key. The usual `spawn` license then spends
    /// that one key; no speculative row-wide sweep is introduced.
    pub(super) fn same_caret_typed_echo(
        &mut self,
        cur: Option<(u16, u16)>,
        cursor_move_observed: bool,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> bool {
        let Some((row, col)) = cur else { return false };
        if !matches!(cfg.style, GlowStyle::RainbowKitty) || cursor_move_observed || self.last != cur
        {
            return false;
        }
        let Some(press_slot) = self.exact_pending_prefix_slot(row, col, col, now, true) else {
            return false;
        };
        let credits = self.type_press_ring;
        self.type_press_ring.retire_before_slot(press_slot);
        let admitted = self.spawn(row, col - 1, row, col, now, cfg, geom, SpawnLane::Visible);
        if !admitted {
            self.type_press_ring = credits;
        } else {
            self.remember_exact_first_cell(row, col - 1, now);
        }
        admitted
    }

    /// A licensed first-cell admission can take the re-anchor branch in
    /// `classify_move`, so the ordinary typed Sweep does not always pass
    /// through `hand_v2_typed_sweep`. Carry the already-laid glyph into its
    /// dormant content run before the next key joins it. No ribbon event or
    /// extra credit is minted here; the caller already laid and paid for it.
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
        let mut glyphs = [' '; RECENT_TYPED_RUN_CAP];
        glyphs[0] = glyph;
        self.recent_typed_run = Some(RecentTypedRun {
            row,
            col0: col,
            len: 1,
            glyphs,
            at: now,
        });
    }

    /// THE HIDDEN/PARKED-CARET ECHO LANE (2026-08-30, the TUI total-suppression
    /// fix): a TUI whose repaint bracket leaves the DEC cursor HIDDEN across
    /// frames, or visible but PARKED away from the caret (CUP 1;1 before
    /// show), never presents a cursor move — every keystroke's echo mutates
    /// cells the move lane cannot see, the ledger freezes (licensed AND
    /// declined), and the effects are simply absent. Measured on the
    /// fc_hidden/fc_parked fixture variants: 25 keys, licensed frozen,
    /// declined frozen, zero band pixels.
    ///
    /// When that exact shape holds — no cursor move observed this tick, the
    /// cursor hidden or at a DIFFERENT row than the print anchor, and a FRESH
    /// typed stamp banked (a real key awaiting its echo) — the observed print
    /// run's endpoint is the honest mutation site of the licensed move, and
    /// the trail anchors THERE: the previous endpoint on that same row is the
    /// launch cell, the new endpoint the landing, and [`Self::spawn`] judges
    /// it under the full license law (the fresh stamp it consumes IS the
    /// keystroke correlation). Program-only output banks no stamps, so it
    /// spawns nothing here — this lane is gated on the typed bank BEFORE
    /// `spawn`, deliberately, so a hidden-cursor build log does not spam the
    /// decline ledger at frame rate.
    ///
    /// Bounds: same-row FORWARD end advance only, at most
    /// [`Self::RAINBOW_TYPED_SWEEP_MAX`] cells — the typed-echo shape. A row
    /// rewrite that lands elsewhere (the spinner row) only re-seeds that
    /// row's memory.
    ///
    /// ROW DISCRIMINATION (the refute round's stray): a fresh stamp alone is
    /// keystroke CORRELATION, not row IDENTITY — a status row growing every
    /// 150 ms under 90 ms typing keeps every advance inside some stamp's
    /// freshness window. Two refusal arms, one reason
    /// ([`Self::DECLINE_PROGRAM_ROW`]):
    /// * TAINT — a row observed advancing its end while no user gesture was
    ///   fresh is branded a PROGRAM row ([`AnchorRow::tainted`]) for the
    ///   entry's lifetime (a spinner advances keylessly all day; the input
    ///   row only advances with keys) — except the established echo row,
    ///   the current [`Self::last_anchor_sweep`] holder, which a licensed
    ///   anchored echo already identified and which an UNDELIVERED paste
    ///   (its arrival stamp revoked at enqueue) advances keylessly — and
    ///   the row a DELIVERED insert's echo lands on;
    /// * SPOKEN-FOR — while a different row's licensed echo is younger
    ///   than the stamp window ([`Self::last_anchor_sweep`], and
    ///   [`Self::last_licensed_row`] — the visible lane's licensed echoes
    ///   count too), no other row may spend a stamp — this
    ///   refuses a program row's brandless FIRST advance (a counter
    ///   crossing a digit width) landing mid-burst, on a hidden frame
    ///   inside an Ink park gap included.
    ///
    /// Refused advances spend no stamps, and the input echo those stamps
    /// belong to still lights.
    ///
    /// One more refusal under the same reason, on the VISIBLE-parked arm
    /// only (a drawn caret on another row, [`Self::observe_caret_drawn`]): a
    /// MINIBUFFER — a status row whose print run ends on a glyph no live
    /// press typed (zle's i-search `<glyph>_`, its fake cursor last). That
    /// advance IS the keys' echo, so it forgets their presses instead of
    /// keeping them (see the gate's own comment). A program that parks a
    /// VISIBLE caret and draws its own fake cursor after each echoed key
    /// goes dark on this arm: the trade-off the gate's comment names.
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
        if seq == self.print_anchor_seen {
            return;
        }
        self.print_anchor_seen = seq;
        let (slot, prev, mut tainted) = match self.anchor_row(ar) {
            Some((i, mem)) => (Some(i), Some(mem.end), mem.tainted),
            None => (None, None, false),
        };
        // The brand as it stood BEFORE this advance: a row branded by an
        // earlier keyless advance is a program row and no receipt may claim
        // its hops; a row branded by THIS advance may be a delivered
        // insert's echo whose receipt is microseconds behind the frame
        // ([`PendingHop`]), and the retro-lay lifts the brand.
        let was_tainted = tainted;
        // ROW DISCRIMINATION (the refuter's stray): a row observed advancing
        // its forward end while NO user gesture was fresh is a PROGRAM row —
        // a spinner, an elapsed timer, a token counter — and is branded so
        // BEFORE any gate below can return early. The brand is judged on
        // every anchor sample (visible-cursor ticks included) and is sticky
        // for the entry's lifetime; without it, a status row growing every
        // 150 ms under 90 ms typing keeps every advance inside a typed
        // stamp's freshness window and each one spends an input echo's stamp.
        // `move_licensed` (any fresh user gesture, not just typed stamps) is
        // deliberately the WIDER exemption: a Tab-completion or Enter can
        // legitimately advance the true input row without a typed stamp, and
        // branding the input row would darken every later echo on it.
        //
        // THE ESTABLISHED ECHO ROW IS EXEMPT: the brand exists to
        // discriminate program rows FROM the echo row, so it must never
        // execute the echo row itself.
        // An UNDELIVERED paste lands here unlicensed — the host stamps its
        // gesture at the input boundary and revokes it in the same
        // event-loop turn (neither the ordered-FIFO enqueue nor the
        // detached fallback is delivery, see `input_paste`; the completed
        // write re-arms the DELIVERED insert, which has its own exemption
        // below) — and branding the input row for that one keyless-looking
        // advance would refuse every later typed echo on it as
        // `DECLINE_PROGRAM_ROW` until a scroll or reset tore the coordinate
        // space down: the TUI anchor dead. The
        // row currently holding [`Self::last_anchor_sweep`] earned its
        // identity through a LICENSED anchored echo — the one thing a
        // spinner/status row can never do (that field has exactly one
        // writer, the licensed anchored spawn below, and the taint +
        // spoken-for arms refuse a program row before it) — so an
        // unlicensed advance on it re-seeds the endpoint and nothing more.
        // Identity, not freshness, on purpose: the repro's paste lands
        // after a >stamp-window quiet gap, where any freshness-scoped
        // exemption is already expired. An undelivered advance itself still
        // spawns nothing (no license term is fresh); only the branding is
        // skipped.
        let established_echo_row = self.last_anchor_sweep.is_some_and(|(row, _)| row == ar);
        // THE PENDING-WRAP PRINT (the xterm model's last column):
        // a glyph printed at the pane's last column leaves the DEC caret ON
        // that column with the grid's deferred wrap pending — no cursor move
        // is ever observed for it, and the print anchor (which counts the
        // pending wrap, one past the last glyph) sits exactly one past the
        // caret at the pane's edge. The visible caret parked there is the
        // row's own witness (it IS the hand's row), so this shape opens the
        // anchored lane beside the hidden and parked ones, and is neither
        // branded nor refused for want of an established row.
        let pane_col1 = {
            let (col0, cols) = self.pane_span(geom);
            col0.saturating_add(cols)
        };
        let wrap_parked = cur.is_some_and(|(r, c)| {
            r == ar
                && usize::from(c).saturating_add(1) == usize::from(ac)
                && usize::from(ac) == pane_col1
        });
        // THE ROW SPAWN LICENSED ON THIS VERY TICK: the visible lane judged
        // this row's advance first (a stalled batch licensed by the
        // in-flight pool with no stamp fresh), and `move_licensed` cannot
        // say so — without this the merged frame brands the input row a
        // program row for the coordinate space's lifetime, and the first
        // hidden or parked frame on it afterwards refuses every anchored
        // echo `program-row`.
        let licensed_this_tick = self
            .last_licensed_row
            .is_some_and(|(row, at)| row == ar && at == now);
        // THE IN-FLIGHT BATCH on the anchored lane: the row's end advances
        // with no stamp fresh and unpaid presses that pay for it (a batch,
        // or one press for its own +1 — a hidden-caret TUI's single stalled
        // key) — a hidden-caret TUI draining a stall. Licensed ONLY on the
        // ESTABLISHED ECHO ROW: the unpaid pool carries no row identity of
        // its own (a spinner, a token counter, an elapsed timer advance
        // inside ten seconds too), and the one row whose identity a
        // licensed anchored echo already proved is the one that may spend
        // it. A program row is branded and refused exactly as before; a
        // never-established row's first stall is refused too (recorded).
        let in_flight = (established_echo_row || wrap_parked)
            && prev.is_some_and(|pc| {
                ac > pc && self.unpaid_typed_echo(ar, pc, ar, ac, now, cfg, geom)
            });
        // THE DELIVERED INSERT'S ROW: a drop as the first
        // action on a never-typed input row must not brand that row either —
        // its advance is the insert's echo when the delivered stamp is fresh,
        // the advance fits it, no other row's licensed echo is younger than
        // the stamp window (spoken for), and the row is the insert's by
        // identity (`insert_row_identity`: the established row, or the
        // insert's whole width). A spinner advancing one digit inside the
        // two-second window is none of those, and is branded as before.
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
            },
        );
        // A visible cursor move owns this tick's PTY delta — the move lane
        // already judged it (licensed or declined); double-judging the same
        // output through both lanes would spend two stamps per key.
        if cursor_move_observed {
            return;
        }
        let hidden = cur.is_none();
        let parked = cur.is_some_and(|(r, _)| r != ar);
        if !(hidden || parked || wrap_parked) {
            return;
        }
        // THE INSERT'S REWRITE under a hidden caret: the TUI
        // swapped the inserted text for a placeholder and the row's end
        // pulled BACK inside the span the delivered insert laid, keylessly.
        // Before the typed gate — a Backspace or kill inside the span keeps
        // its own class (`insert_rewrite` refuses while one is fresh), and a
        // retreat with no insert behind it re-seeds the endpoint and nothing
        // more, exactly as before.
        if let Some(pc) = prev
            && ac < pc
            && let Some(cells) = self.insert_rewrite(ar, pc, ar, ac, now, cfg)
        {
            self.retract_insert(ar, ac, cells, now);
            self.last_move = Some(now);
            self.last_anchor_sweep = Some((ar, now));
            return;
        }
        // The correlation gate: a banked fresh typed stamp — a real key whose
        // echo is pending — a fresh DELIVERED INSERT, or the in-flight
        // batch on the established echo row opens this lane;
        // nothing else does, so a hidden-cursor build log still spams no
        // ledger (program output banks no credits). A forward advance
        // refused here is remembered for one delivery receipt still in
        // flight ([`PendingHop`]).
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
        // The typed coalesce cap — raised to the delivered insert's OWN
        // width plus the typed credits behind it ONLY for the row the insert
        // admits (a 63-cell path is one echo there): raised for every row,
        // a typed stamp would license a 33..reach-cell advance on a status
        // row the typed cap returns silently on, and `spawn` would read that
        // hop as a re-anchor, light its landing and move the echo row
        // there. The same discipline for the cap itself: a row whose
        // identity NO licensed anchored echo has proven keeps the 32-cell
        // bound (`ANCHOR_UNPROVEN_ROW_MAX`) — a program row's first advance
        // past it beside a fresh stamp returns silently — and only the
        // ESTABLISHED echo row takes the full cap, where a stalled batch of
        // up to 128 cells is its own.
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
            // Past the cap with a typed stamp fresh and no receipt yet: the
            // drop's echo observed one frame before its receipt, refused
            // here for one delivery to claim ([`PendingHop`]).
            if !was_tainted {
                self.remember_refused_hop(ar, pc, ac, now, cfg);
            }
            return;
        }
        // The row-discrimination refusal, two arms, one law — the row must be
        // the ECHO's row, not merely fresh-adjacent:
        // * the taint brand (this row has advanced keylessly; it is a program
        //   row for the entry's lifetime);
        // * the spoken-for hold (a DIFFERENT row's licensed anchored echo is
        //   younger than the stamp window — the fixture crossing `(9)`→`(10)`
        //   is a program row's FIRST advance, brandless by definition, but it
        //   lands mid-burst while the input row is re-lighting every echo).
        // A refused row never reaches `spawn`, so it can neither mint light
        // nor consume the banked stamp — the stamp survives for the input
        // echo it belongs to. Logged (the only decline this lane emits)
        // because a program row advancing inside a typed stamp's freshness
        // window is the contested case worth a ledger row; program-only
        // output with no stamps banked still returns silently above,
        // spamming nothing.
        // THE HOLD READS EITHER LANE (the spinner crossing inside a hidden
        // park gap): `last_anchor_sweep` has exactly one writer, the
        // licensed ANCHORED spawn, so an input row whose every echo was
        // judged on the VISIBLE lane (Ink's park under a hide the caret
        // reappears from one cell on — a hide-bridged `+1`) never
        // establishes itself here, and a status row's brandless first
        // advance landing inside the next key's stamp window on a hidden
        // frame would be licensed `key` — the stamp and the credit spent on
        // it, the key's real echo refused `no-fresh-hint`. `last_licensed_row`
        // is written by every licensed spawn on both lanes (and the insert's
        // lay and retract), so the hold stands on the last row the hand was
        // licensed on, whichever lane proved it. A program row never spends
        // a typed stamp.
        let spoken_for = self
            .last_anchor_sweep
            .is_some_and(|(row, at)| row != ar && younger_than_stamp(at))
            || self
                .last_licensed_row
                .is_some_and(|(row, at)| row != ar && younger_than_stamp(at));
        if tainted || spoken_for {
            // Logged when CONTESTED — a typed stamp is fresh, or the advance
            // is the insert's identity width; a spinner ticking inside the
            // two-second insert window on its own is refused silently, so it
            // cannot push the drop's own verdict out of the 32-row ring.
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
        // THE MINIBUFFER BELOW A VISIBLE CARET (zsh's Ctrl-R, 2026-09-23 —
        // the owner's `failing bck-i-search: clad_` slab). zle keeps the caret
        // VISIBLE on the history match and echoes each key on a status row
        // below it as `<glyph>_`, one write per key: the print run ends one
        // past its FAKE cursor, so the run's last cell is the `_`, never the
        // key. Laid, every key put a cell under the `_`, and the failing key's
        // whole-row rewrite (`failing ` prepended, the end jumping nine cells
        // on one credit) laid the credit-starved re-anchor's landing there —
        // a slab under `_` and the blank after it whose tall top and hot edge
        // showed as a stub under the prompt's text on the caret's row. So on
        // a PARKED VISIBLE caret — never the pending-wrap shape, never a
        // delivered insert — the lane lays a key's echo only where the run
        // ends on a live press's own glyph ([`Self::glyph_before`]: the
        // host's anchor glyph, else the caret row's probes; a wrapped match
        // puts the status row two rows down, where only the anchor glyph
        // sees it). VISIBLE is the host's word, not the lane's: the
        // single-pane window hands a DECTCEM-hidden caret over too (a hidden
        // cursor is still a caret, for the pet) and says so
        // ([`Self::observe_caret_drawn`]), so a hidden-caret TUI's echo on
        // another row keeps the lane it has on the composed, focus and
        // headless paths, which hand over no caret at all — its rewrite ends
        // wherever its row ends, a fake cursor of its own included. Unknown
        // — no glyph for that row, a glyph-less press (a wide glyph, an IME
        // commit, a count-only host), an empty pool — keeps the lane exactly
        // as it was. The refused echo is the keys' own, arrived where it
        // cannot be lit, so their presses are forgotten (the in-flight law):
        // left banked they would pay for the match row's next hop, and a
        // stamp with no credit behind it licenses no Rainbow Kitty light.
        // THE TRADE-OFF: a program with a VISIBLE caret parked off its echo
        // row that draws its OWN fake cursor after each echoed key (`k█`)
        // ends every run on that fake cursor, so its echoes go dark here
        // instead of laying one cell right of the key; no recorded program
        // shows that shape.
        if parked
            && !self.caret_hidden
            && insert_admits.is_none()
            && let Some(printed) = self.glyph_before(ar, ac, now)
            && self.type_press_ring.foreign_to_live(now, printed) == Some(true)
        {
            self.log_decline(now, (ar, pc), (ar, ac), Self::DECLINE_PROGRAM_ROW);
            self.forget_typed_credits(now);
            return;
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
            // The licensed echo establishes/refreshes this row as THE echo
            // row (the spoken-for hold above): during a burst the input row
            // re-lights every echo, so no other row can spend a stamp.
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

    /// RELEASE A HELD PARK ([`HeldPark`]) by the move that reached the seam,
    /// or leave it for the custody arm and the flush (`None`). Rainbow Kitty
    /// only, inside the park's window (fresh; a cross-row park also needs
    /// its source confirmed). Three releases, mutually exclusive:
    ///
    /// THE RETURN on the visible lane — a forward move from the park's
    /// landing past its origin. Judged as `origin -> target` by the whole
    /// body: the licence gate on the key's fresh stamp (or the in-flight
    /// pool), the classifier, the spend, the sweep and the `Move` from the
    /// origin (where the engine's mirror still sits, the park's `Move`
    /// having been withheld), the ring row. FUNDED ONLY BY THE PRESSES
    /// BANKED AT OR BEFORE THE PARK ([`Self::park_return_paid`]): the
    /// return's cells are the glyphs the rewrite carried, in flight when
    /// the park was observed; a key pressed after the park (vim's `$` a
    /// beat after its `b`) must not pay for them, or the return painted two
    /// cells no key typed. Or a delivered insert's echo shape from the
    /// origin ([`Self::insert_echo`]) — the insert arm then lays
    /// `origin -> target` and pays any surplus from the pre-park presses.
    ///
    /// THE RETURN SEEN ON THE ANCHORED LANE — Ink's second bracket (hide,
    /// rewrite the row, show) sampled mid-bracket, the caret hidden and the
    /// print anchor advanced from the park's origin (the row's remembered
    /// end, `pc` on this lane) to the row's new end. The park is released
    /// and the hop judged as its own `origin -> end` on the key's stamp (or
    /// the pool), so the show frame's `landing -> end` is a same-row
    /// declined relocation that leaves the mirror correct. Flushed instead,
    /// the park was a typed re-anchor on the prompt cell and the mirror
    /// stayed parked at the landing — the next key typed into a stall
    /// replayed its `Typed` there. `park_return_paid` is deliberately not
    /// asked: on this lane the row's print is the witness, and a press made
    /// after the park whose glyph echoed in the same rewrite may pay.
    /// `pc == p.origin` confines the arm to end-of-line typing; a mid-line
    /// insert still flushes.
    ///
    /// THE SAME-END RETURN — a repaint that carried no new glyph (a
    /// spinner/status frame between the park and the key's echo) rewrites
    /// the row to its OLD end, `landing -> origin`. The mirror never left
    /// the origin and no glyph was typed at the landing, so this is not a
    /// re-anchor: the park is dropped — nothing consumed, no `Move`, no ring
    /// row — and the stamp (or the pool) survives for the echo it belongs
    /// to. Flushed instead, the park spent the key's stamp on the prompt
    /// cell and the key's own `+1` was refused dark.
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
                    && (self.park_return_paid(&p, cc, now)
                        || self
                            .insert_echo(p.row, p.origin, cr, cc, now, cfg)
                            .is_some())
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
        let (pr, pc) = match self.release_held_park(lane, (pr, pc), (cr, cc), now, cfg) {
            Some(ParkRelease::Cancelled) => return false,
            Some(ParkRelease::Return { pr, pc }) => (pr, pc),
            None => (pr, pc),
        };
        // More foreign-row redraws cannot turn the pending key into footer
        // light. Retain its source, updating only the observed landing. The
        // existing credit patience bounds this custody; a class-changing
        // gesture or expiry discards it without spending on the foreign row.
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
        // THE FLUSH: any other move judges the held park first, at the
        // park's own clock — today's verdict, emitted later.
        if let Some(p) = self.held_park.take() {
            self.flush_park(p);
        }
        self.spawn_judged(pr, pc, cr, cc, now, cfg, geom, lane, SpawnCall::Live)
    }

    /// Judge a held park ([`HeldPark`]) through the ordinary body at the
    /// park's own clock and under the config and geometry it arrived with:
    /// the licence gate re-evaluates at `p.at` (the park's stamp was fresh
    /// then; an in-flight park runs the refusal branch — refused and the
    /// pool forgotten, today's verdict), `classify_move` pops the park's own
    /// stamp (oldest-first — later stamps are in the future at `p.at`),
    /// lays nothing forward, classifies a wide retreat or a content-proved
    /// short wrap as a re-anchor (an unproved one-cell park as `typing`, a
    /// two-cell one as a jump), hands v2 its
    /// `Move` dated `p.at`, writes today's ring row.
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
        // JUDGED AGAINST THE BANKS AS THEY STOOD AT THE PARK: every
        // freshness read is `now.saturating_duration_since(t) <= window`,
        // true for any `t >= now`, so a key typed AFTER the park (the
        // stalled key's rewrite held on the pool until the next tick) was
        // in the bank when the flush ran at `p.at` — the park was licensed
        // as a typed re-anchor on the NEW key's stamp (the prompt cell lit,
        // the stamp popped, a credit spent) and the new key's own echo was
        // refused. The later stamps and presses are set aside for the
        // judgment and put back after it; `spawn_judged` never banks, so
        // the head and every taken index are unchanged, and a refusal's
        // forget drops only the presses in flight at the park.
        let later_stamps = self.type_hint.split_off_after(p.at);
        // …and the events the judgment mints — the landing sweep, the
        // `Move` — are the park's own, ordered before the keys pressed
        // since (`rk::Engine::set_flush_clock`).
        // ONLY FOR A PARK THE RIBBON WILL READ AS A RE-ANCHOR (2026-09-22,
        // the merge with main's `RE_ANCHOR_MIN_CELLS`, plus a short park
        // whose source row was proved to follow its text). The ordering exists
        // for the composer's box-growth wrap, where the park's own landing
        // sweep and `Move` must be replayed before the keys pressed since,
        // so the relaid word takes the walk it had. Under the ribbon's own
        // floor without that content proof the move is not a re-anchor at
        // all — a one- or two-cell backward park flush is the mirror moving
        // — and reordering there
        // HELD the key that followed it instead of laying its cell: the
        // witness then saw the row's last glyph blank under an armed cell,
        // released it, and `wrapped_composer_band`'s `c2_bs` carried 218
        // row-frames with interior dark runs (measured 2026-09-22; 0 with
        // this scope, and the same 0 main has).
        let reanchor = p.content_followed
            || (p.landing_row == p.row
                && p.origin > p.landing
                && p.origin - p.landing >= rk::ribbon::RE_ANCHOR_MIN_CELLS);
        self.v2.set_flush_clock(reanchor.then_some(p.at));
        let licensed = self.with_presses_banked_through(p.at, |glow| {
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
        });
        if licensed && p.content_followed {
            self.v2
                .prove_short_reanchor((p.row, p.origin), (p.landing_row, p.landing), p.at);
        }
        self.v2.set_flush_clock(None);
        self.type_hint.merge(later_stamps);
    }

    /// Run `judge` against the press ring as it stood at `at`: the presses
    /// banked after `at` are set aside and put back after — a judgment at
    /// a past clock must not spend a press typed since ([`HeldPark`]'s
    /// flush; a Return's or an arrow's forget edge, which spares the glyph
    /// typed after its key). `judge` never banks, so the head and every
    /// taken index are unchanged.
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

    /// HOLD a same-row backward move as a park ([`HeldPark`]) for its
    /// return, with the config and geometry it arrived under and whether a
    /// glyph sits at its landing cell.
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

    /// The clock a swept cell is BORN at: the key's, floored one stamp
    /// window before the echo so a debounced TUI's late echo is not born
    /// mid-retract (the echoing frame shows the run lit whatever the
    /// stall).
    pub(super) fn sweep_born(key_at: Instant, now: Instant) -> Instant {
        key_at.max(
            now.checked_sub(Duration::from_secs_f32(Self::TYPE_HINT_FRESH))
                .unwrap_or(now),
        )
    }

    /// Whether a GLYPH sits at the cell left of `col` on `row` as THIS
    /// frame's row probe shows it ([`HeldPark::landing_glyph`]): the probe
    /// is fed before the tick that judges the move, so at park time it is
    /// the row as the park found it. No probe, or one for another row,
    /// reads as a glyph (unknown keeps the sweep).
    pub(super) fn landing_glyph_at(&self, row: u16, col: u16) -> bool {
        self.row_cur_meta.is_none_or(|m| {
            m.row != row
                || col
                    .checked_sub(1)
                    .is_some_and(|left| !rk::witness::unit_at(&self.row_cur, left).is_blank())
        })
    }

    /// Whether the presses banked AT OR BEFORE the park pay for the return's
    /// cells `origin..cc` ([`HeldPark`]): outright, or under the
    /// coalesce's own share rule (two or more presses, three quarters of the
    /// cells) so a rewrite one press short of a real burst still returns.
    pub(super) fn park_return_paid(&self, p: &HeldPark, cc: u16, now: Instant) -> bool {
        let cells = usize::from(cc.saturating_sub(p.origin));
        let before = if p.cross_row {
            self.typed_credits_within(now)
        } else {
            self.type_press_ring.cells_where(now, |t| t <= p.at)
        };
        before >= cells || Self::share_rule(before, before, cells)
    }

    /// Flush a held park before an edge that would have judged it — a key
    /// class change, a scroll that carries it off the top, a band move that
    /// carries it past the band's edge, a stale hidden boundary, a teardown
    /// — so a park never outlives the row it was held on.
    pub(super) fn flush_held_park(&mut self) {
        if let Some(p) = self.held_park.take() {
            self.flush_park(p);
        }
    }

    /// Whether a same-row BACKWARD move is a PARK candidate ([`HeldPark`]):
    /// Rainbow Kitty, the visible lane, no one-shot class fresh
    /// ([`Self::one_shot_licensed`]) nor a kill or a reflow (each keeps its
    /// own verdict at once), and something in flight behind it — a press
    /// (the key's own, its stamp fresh and its credit unpaid, or the
    /// stalled pool's — at the flush it is judged as the keyless backward
    /// hop it was, refused and the pool forgotten) or a fresh DELIVERED
    /// INSERT (a dropped path takes Ink's park + rewrite route too, and
    /// judged from the park landing its rewrite is wider than the insert's
    /// reach by the prefix's width — the whole drop dark).
    /// Any width (`dc >= 1`): the measured Ink trace parks by one and two
    /// cells before it parks wide, and a held park changes no verdict — it
    /// only defers the v2 `Move` by at most one stamp window.
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
            // A LIFTED WORD is the composer's re-wrap, not Ink's park: the
            // word it left went up to the row above, and no return follows
            // ([`Self::lifted_word`]).
            if cc >= pc || self.lifted_word(pr, pc, cr, cc).is_some() {
                return false;
            }
        } else if pr.abs_diff(cr) != 1
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
        // A press in flight — the stamp's own, or the pool's (a fresh stamp
        // with nothing unpaid holds nothing: the park is refused and
        // forgotten like the keyless control, else its flush lights the
        // landing on a stamp that has already been paid)
        // — or a delivered insert awaiting its echo.
        self.typed_credits_within(now) >= 1 || self.insert.fresh(now).is_some()
    }

    /// A cross-row park needs content evidence: the exact source prefix
    /// sampled on its preceding frame is still present now. The resident
    /// ribbon's row sample supplies that second observation. A real wrap,
    /// moved input box, unknown row or changed prefix keeps its existing
    /// immediate verdict; no program-name or glyph-class heuristic is used.
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

    /// **THE KEY PUSHED THE ROW'S TEXT DOWN WITH THE CARET** (2026-09-23,
    /// the owner's Claude Code composer: a word typed INTO the text before
    /// a word at the end of row `r`; the whole insert stayed dark). Claude
    /// Code 2.1.280 re-wraps its box per key: when the key makes the word
    /// at the caret too long for row `r`, Ink ERASES row `r` from the caret
    /// (`ESC[K`) and writes the key's glyph and the word from the
    /// continuation row's indent, caret after the key's glyph:
    /// `(36,112) → (37,3)` for `a[image #1]` at 120 columns. Row `r`'s
    /// prefix is untouched, so the cross-row park's content test
    /// ([`Self::park_source_unchanged`]) read it as a background footer
    /// repaint and HELD the key's move, and `spawn` swallowed every later
    /// move for the park's whole patience: no verdict, no cell.
    ///
    /// Three observations on the glass, all required, tell the key's own
    /// echo from a footer: (1) the source row's TAIL — every column from
    /// the origin to the end of its old ink — was erased this frame, (2)
    /// that exact tail follows the landing glyph on the row below, and (3)
    /// the landing glyph is an unpaid press's own. A footer can erase the
    /// source tail and print the SAME glyph as the key, so the moved tail
    /// is the content-identity proof. With it the exact credit may keep its
    /// full in-flight patience: a 300 ms Claude Code echo must not be held
    /// dark merely because the classifier's 250 ms stamp expired.
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

    /// THE LICENCE CLASS: the classifier's verdict restated in v2's
    /// vocabulary, read by the forget edges for every style — v2 never
    /// re-derives one (T1 lives upstream). A kill chord's own caret retreat
    /// (^W / ^U) is the kill's, not a navigation gesture: inert here, the
    /// drain is the `Kill` event. A typed echo, a Backspace retreat (its
    /// `Erase` went out at the key), a coalesced Backspace run and a
    /// settled resize (§6.1: "reflow licenses nothing") are all inert in
    /// v2: the caret mirror moves, nothing flies, nothing is abandoned.
    /// `bs_pair` is the quench hint PEEKED before `classify_move` consumed
    /// it on this very move, so a quench-licensed jump keeps its class. A
    /// delivered insert never reaches here (its same-row sweep is laid by
    /// the insert arm above the licence gate); a Tab whose completion went
    /// CROSS-ROW spent its gesture as `Synthetic`, and the insert stamp
    /// armed beside it goes with it.
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

    /// **THE FORGET EDGES** (the in-flight law): a licensed move the
    /// presses in flight cannot explain forgets them — the mirror of the
    /// echo ledger's own clears (`Engine::echo_bridge`). A NON-TYPED
    /// licence (an arrow's hop, a Return's row change, a scripted gesture,
    /// a kill's retreat) closed the row the presses were on; a ROW CHANGE
    /// no typed stamp paired with (a wrap and a scroll-translated echo keep
    /// their stamp and their pool); a forward hop the share rule refused
    /// (`no-credits` — the pool did not describe it); a typed-paired hop
    /// past the cap (a re-anchor). Every one at the licensed MOVE, not the
    /// key — the in-flight echoes that precede a Return's own move are
    /// still licensed by their credits (a kill forgets at the key,
    /// `note_kill`: its erase is the line's content going). A Return's, an
    /// arrow's, a composer newline's or a moving kill's move forgets only
    /// the presses banked AT OR BEFORE its own key (`spare`): a glyph typed
    /// after the key, before its echo landed, is type-ahead onto the new
    /// row and keeps its credit for its own echo — exactly as the held
    /// park's flush spares the presses typed after the park.
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

    /// A typed echo can resume beside cells whose natural swoosh has already
    /// removed them. The last run's exact grid glyphs (including a typed
    /// trailing space) are kept outside the live ribbon. Only this next
    /// licensed, adjacent typed echo can read them, and only while the same
    /// row still holds every glyph. Re-lay once on that key frame if any old
    /// cell has expired; idle frames do no scan and no work is scheduled.
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
        if joined && let Some(run) = old {
            let ribbon = self.v2.ribbon();
            let mut standing = [false; RECENT_TYPED_RUN_CAP];
            let cohorts = ribbon.cohorts();
            // Cohorts are appended with wrapping IDs and only removed, never
            // reordered. Compare offsets from the oldest live ID so even an
            // ID wrap preserves binary-search order. One lookup per relevant
            // cell avoids a cohorts scan for every key in a long typed run.
            let first_cohort = cohorts.first().map_or(0, |coh| coh.id);
            for cell in ribbon.cells() {
                if cell.row != row
                    || !(run.col0..col0).contains(&cell.col)
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
                {
                    standing[usize::from(cell.col - run.col0)] = true;
                }
            }
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
            }
        }
        self.v2.on_event(rk::Event::Sweep { row, col0, col1 }, born);
        if joined {
            if let Some(run) = self.recent_typed_run.as_mut() {
                let start = usize::from(run.len);
                for (i, col) in (col0..col1).enumerate() {
                    run.glyphs[start + i] =
                        self.row_cur.get(usize::from(col)).copied().unwrap_or(' ');
                }
                run.len += col1 - col0;
                run.at = now;
            }
        } else if usize::from(col1 - col0) <= RECENT_TYPED_RUN_CAP {
            let mut glyphs = [' '; RECENT_TYPED_RUN_CAP];
            for (i, col) in (col0..col1).enumerate() {
                glyphs[i] = self.row_cur.get(usize::from(col)).copied().unwrap_or(' ');
            }
            self.recent_typed_run = Some(RecentTypedRun {
                row,
                col0,
                len: col1 - col0,
                glyphs,
                at: now,
            });
        } else {
            self.recent_typed_run = None;
        }
    }

    /// HAND V2 THE LICENSED MOVE (seam point 1): the typed sweep, the
    /// re-anchor's landing, the fold's cells, the kill's retreat, then the
    /// `Move` — in that order, the ribbon's contract — and the ring row.
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
        // A delivered insert never reaches here: its same-row sweep is
        // laid by the insert arm above the licence gate under
        // `rk::Licence::Insert`.
        // A TYPED ECHO hands v2 its glyph cells too ([`rk::Event::Sweep`]):
        // the keys lay at the caret the tick replays with,
        // so a batched echo (the classifier's `rainbow_coalesce` verdict,
        // paid for by the press-credit ring) and a key whose echo landed
        // a frame late would leave their glyph cells dark for good. The
        // ribbon lays only what no live cell owns, so a key whose echo
        // shares its tick is untouched.
        // A TUI re-anchor (a typed stamp beside a long same-row hop
        // with no credits to pay for it — vim's `w`) lights only the
        // landing, exactly as v1 did; the coalesced echo outranks it.
        // The sweep is DATED at the KEY's clock — the stamp for a
        // per-key echo, the OLDEST unpaid press for a stalled batch —
        // because that clock is what the engine's echo
        // ledger partitions by: the presses older than it pay the hole
        // to its left, exactly, and a batch dated at its oldest press
        // is SPENT (its tail kept for the next key) rather than
        // forfeited. The RIBBON's birth is floored by the engine at one
        // stamp window before the echo (`rainbow_kitty::SWEEP_BIRTH_FLOOR_S`),
        // so the echoing frame shows the run lit whatever the stall.
        // …and only for a TYPED-PAIRED hop: the geometric `mv.typing`
        // alone is `dist <= 1`, which a one-shot key's +1 also satisfies
        // — the first → after a word would extend the word's cohort with
        // a full-price typing cell, and a Backspace or a moving kill
        // followed by a keyless program +1 (a spinner, a status redraw)
        // would light it as typed. An arrow's hop
        // keeps its wake (`Licence::Nav`), a quench/kill/return-licensed
        // keyless +1 keeps its inert move; every stamp, in-flight or
        // older-batch sweep is typed-paired and unchanged.
        if mv.typing
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
        } else if licence != rk::Licence::Typed || cr != pr || cc < pc {
            self.recent_typed_run = None;
        }
        // **A SOFT-WRAPPED CARET LAYS ITS KEY WHERE THE GLYPH IS**
        // ([`Self::soft_wrapped_caret`]): the key's glyph stands at the
        // ORIGIN and the landing row's head is the blank indent, so the
        // one cell the spend paid for is `(pr, pc)`, swept on the same
        // clock as the landing and the fold's cells below, which it
        // replaces. Laid at `landing − 1`, it was the owner's stub in the
        // indent, and the real glyph on the row above stayed dark.
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
        // **THE RE-ANCHOR'S LANDING IS SWEPT** (the held park's dark
        // landing). A typed re-anchor lays exactly ONE cell, the landing,
        // and the credit it spends above is spent on exactly that cell —
        // but nothing else in the seam lays it: the landing is lit by the
        // KEY's own `Typed` replay, which folds back from the caret mirror
        // the licensed `Move` had just moved, and with a held park that
        // mirror can be a frame or a stamp window behind (the box-growth
        // wrap's own key is replayed against the PRE-wrap caret, so its
        // glyph cell lands on the abandoned band, where the content
        // witness retires it, and the landing the flush licenses is never
        // laid — one dark cell at the fold, exactly the shape
        // `rk::Event::Sweep` exists for). Swept here on the same predicate
        // as the spend, dated at the KEY and floored one stamp window back
        // like the fold's; the ribbon lays only what no live cell owns, so
        // a re-anchor whose `Typed` already laid its landing is
        // byte-identical.
        // The landing folds at the FOCUSED PANE's first column, never the
        // grid's: `Ribbon::lay` — the replay path
        // this sweep stands in for — wraps at `set_pane`'s edges, so
        // "the cell before its first column is the previous row's LAST
        // pane cell, never the neighbouring pane's", while `Ribbon::sweep`
        // clamps to the grid alone and would have laid `cc - 1` verbatim:
        // a live cell in the split beside the one the hand is typing in.
        // REFUSED there rather than folded: a re-anchor landing on the
        // pane's own first column is always the backward (parked) shape,
        // and the row above's last pane cell is a cell main's replay never
        // laid for it — the credit is spent (as it is at the grid's edge)
        // and no light is minted.
        // …and a FLUSHED PARK sweeps its landing only if a glyph sat
        // there when it was held: an Ink park to the
        // input's start lands beside the prompt, and the rewrite that
        // arrives after the window must not light it.
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
            // The ordinary one-cell typed re-anchor can lay the first
            // wrapped-row glyph here without taking the usual typed-Sweep
            // path. Preserve its already-licensed content for a later
            // same-row continuation after the ribbon's natural retirement.
            if licence == rk::Licence::Typed
                && mv.typing
                && mv.typed_hinted
                && cr == pr
                && pc.checked_add(1) == Some(cc)
            {
                self.remember_exact_first_cell(cr, cc - 1, now);
            }
        }
        // THE FOLD'S OWN CELLS (the stalled key whose echo wraps the
        // row): a typed fold reaches v2 as a bare `Move`, which the ribbon
        // treats as inert — a per-key fold is lit only because the key's
        // `Typed` replay folds its cell off the left edge onto the row
        // above, which needs the key's echo to share its tick; a stalled
        // key's `Typed` was replayed at its own tick against the pre-stall
        // caret, so its last-column cell would stay dark for good. The seam
        // sweeps the origin row's tail (`pc..` the
        // pane's edge) and the landing row's head (the pane's first
        // column `..cc`), dated at the key and floored HOST-SIDE at one
        // stamp window before the echo — the engine floors only the
        // `from..to` sweep it matches as the host sweep, which an
        // origin-row sweep never is. The ribbon lays only cells no live
        // cell owns, so a per-key fold whose `Typed` already folded is
        // byte-identical.
        // The sweep covers exactly the cells the fold PAID for
        // (`fold_laid`), ending at the landing and folded onto the origin
        // row — the same cells the key's `Typed { cells }` replay lays,
        // so a wide glyph's fold is the glyph's own width and nothing
        // wider.
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
            }
        }
        // **THE KILL'S RETREAT CARRIES ITS KILL.** A ^U / ^W retreat
        // reaches the ribbon as a `Typed`-licensed same-row backward
        // `Move` BEFORE `poof_scan` (later in this same tick, or a tick
        // later on the caret fallback) mints the `Kill`; with
        // `pending_erase == 0` the ribbon would take the COMPOSER path and
        // `re_anchor` would relay the killed text's last word LEFT of the
        // landing — under `'d '` of a 44-column zsh prompt at the kill
        // itself, under `% ` / Claude Code's `❯ ` at the third key after
        // it — held lit for as long as the hand typed on the row. The
        // retreat's own span is the kill's: emitted here, before the
        // `Move`, so the ribbon takes the erase-retreat arm and drains
        // exactly the cells the caret retreated over. The poof's `Kill`
        // (seam point 2) then stands down for this kill — and this stands
        // down for a kill the poof already minted (the caret fallback
        // answers at the grace; a slow link lands the retreat after it):
        // one kill, one `Kill` (`kill_reported_to_v2`).
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
        // The engine is told BEFORE the `Move` it names: its replay lays a
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
        // …and a LIFTED WORD ([`Self::lifted_word`]): the ribbon carries the
        // word's light up with its text rather than draining it under the
        // caret.
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
        // …and THE SOFT-WRAPPED WORD CAME DOWN ([`Self::carried_word_left`]):
        // the ribbon relays the word a soft-wrapped caret left on its row
        // only on the echo the glass shows carried it off.
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
        // Scored for `trail status` exactly as a v1 spawn is; the v1
        // geometry census below cannot see v2's births, so the ring
        // records the licence rather than a false `off-shape`.
        self.spawns += 1;
        // **A REFUSED SWEEP IS RECORDED AS A REFUSAL.** This is the
        // instrument: `DECLINE_NO_CREDITS` written only from the v1 spark
        // path — which this branch returns before reaching — let every
        // take that visibly TORE report `declined=0
        // last_decline_reason=none` while cells went black, the ring
        // saying `licensed` for a sweep it had just refused.
        //
        // The move IS licensed and its landing IS laid — `spawns` still counts
        // it, and not one admission verdict changes here. What changes is only
        // what the ring is told: when the credit budget and nothing else
        // refused the coalesce (`credit_starved` mirrors every other clause of
        // `rainbow_coalesce` exactly), the swept cells the user typed did not
        // get born, and THAT is the event `ctl trail status` exists to name.
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
        // THE DELIVERED INSERT (Rainbow Kitty only, at this seam like the
        // unpaid press below; `move_licensed` stays the classic trail's
        // lockstep contract). A file drop, ⌘V, the `paste` verb, a Tab
        // completion or a ⌃V is the user's own gesture, but to the PTY
        // stream its echo is program output: the host stamps its gesture
        // at the input boundary and revokes it in the same turn (enqueue
        // is not delivery), so a plain-shell paste was refused
        // `no-fresh-hint` (`origin=3,50 target=3,58`) and the walk
        // restarted after an 8-cell hole. The witness is the host's
        // COMPLETED WRITE (`note_insert_delivered`): a same-row forward hop
        // no wider than
        // the delivered insert plus the typed credits behind it, on a row
        // that is its own (`insert_row_identity`), that no fresher press
        // class explains, is the insert's echo — laid as ONE sweep joining
        // the cohort, and nothing else (no thermals, no click, no momentum,
        // no typed stamp popped: an insert is not typing). Judged BEFORE the
        // licence gate because the gate's classes cannot describe it, and
        // before `classify_move` because a typed stamp beside a 63-cell hop
        // would read it as a re-anchor and lay only the landing.
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
        // THE INSERT'S REWRITE: the program pulled the caret BACK inside the
        // span a delivered insert laid (Claude Code swapping the dropped path
        // for `[Image #1] `), keylessly, with the row probe showing the
        // suffix blank. Judged before the typed classifier too — a glyph
        // never moves the caret left, but a typed stamp beside this retreat
        // would classify it as a re-anchor, move the mirror and retract
        // nothing, leaving lit cells under the blanks right of the caret.
        // Any keyed retreat (Backspace, a kill, an arrow) keeps its own
        // class: `insert_rewrite` refuses while one is fresh.
        if let Some(cells) = self.insert_rewrite(pr, pc, cr, cc, now, cfg) {
            self.retract_insert(cr, cc, cells, now);
            return true;
        }
        // THE LICENSE SEAM, and the whole gate (see [`Self::move_licensed`]).
        // An UNLICENSED move — program output nobody's fingers asked for —
        // returns here, before `classify_move`, before one byte of state
        // moves. Two halves, and the second is as load-bearing as the first:
        //
        // MINT NOTHING. Not a thermal, not a sound cue, not a birth, not a
        // glide sample, not a crown. Strictly stronger than v0.43.0, whose
        // tick spawned on every presented cursor delta and let a cold one-cell
        // advance earn heat — the cold streamer light a bare revert re-opens.
        //
        // CLEAR NOTHING. Earned light is never destroyed by someone else's
        // output: retention is decay + `note_scroll` translation + reset, the
        // v0.43.0 law. The proof era's denial paths did BOTH — they refused to
        // mint (fine) and then called `clear_denied_move_visuals`, which wiped
        // the ribbon the user's own typing had just earned, five measured
        // segments at a time, because a spinner two rows away moved its caret.
        // That wipe is the darkness the owner reported, and it is gone.
        //
        // THE UNPAID PRESS IS A LICENCE TOO (Rainbow Kitty only).
        // `move_licensed` asks whether a key was pressed RECENTLY; a debounced
        // app repaints when it likes, and half a second after the hand paused is
        // routine — the whole batch then declines here, and not one of the cells
        // the user typed is ever born. `move_licensed` itself is deliberately
        // untouched: it is a public lockstep contract with the classic trail
        // (`app_render.rs:635`), so the extension lives at THIS seam, on the ECHO
        // SHAPE, for the one style whose ribbon is a per-cell record of the keys.
        // Program output banks no credits, so a keyless caret walk still declines
        // exactly as before.
        // THE PARK ([`HeldPark`]): a same-row backward move a
        // stamp or the in-flight pool stands behind is HELD for its return
        // — in both branches of the gate, before anything is consumed,
        // spent, forgotten, emitted or scored. It is scored when it is
        // judged: at the flush, or as the return.
        let park = call == SpawnCall::Live
            && lane == SpawnLane::Visible
            && self.park_candidate(pr, pc, cr, cc, now, cfg, geom);
        if park && cr != pr {
            self.park_source_cells.clear();
            self.park_source_cells
                .extend(self.row_prev.iter().take(usize::from(pc) + 1));
            self.park_source_cells.resize(usize::from(pc) + 1, ' ');
        }
        if !self.seam_licensed(now, cfg) && !self.unpaid_typed_echo(pr, pc, cr, cc, now, cfg, geom)
        {
            if cr == pr && cc > pc {
                // A refused same-row forward hop is remembered for ONE
                // delivery receipt that may still be in flight
                // ([`PendingHop`]). The pool is KEPT: it is empty here, or
                // holds one credit against a hop wider than one cell (a
                // one-press +1 would have been licensed), and that credit
                // is the insert receipt's and the next key's ledger bridge.
                self.remember_refused_hop(cr, pc, cc, now, cfg);
            } else {
                if park {
                    // The stalled key's rewrite: held so the return can
                    // spend the pool; no return, and the flush refuses and
                    // forgets exactly as the line below would now.
                    self.hold_park((pr, pc), (cr, cc), now, cfg, geom);
                    return false;
                }
                // A keyless BACKWARD or CROSS-ROW hop is not the echo shape
                // and no key explains it (the ctrl+c clear, a modal's
                // repaint, a pager's exit on a new row, Shift+Enter's box
                // growth): the presses in flight are forgotten with the
                // refusal — the row they were on is gone.
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
        // Read BEFORE the classifier spends it: `glyph_echo` below needs to
        // know whether a press was unpaid when the move arrived, and a
        // coalesced echo's spend empties the pool on this very move.
        let unpaid_before = self.typed_credits_within(now) >= 1;
        let mv = self.classify_move(pr, pc, cr, cc, now, cfg, geom, lane, call);
        // A TYPED RE-ANCHOR LANDING ON A GLYPH NO LIVE PRESS TYPED
        // ([`MoveCtx::landing_foreign`]: zsh's Ctrl-R walking the visible
        // caret along the match row) is not the keys' echo: declined here,
        // before the thermals, the abandonment and the licence takes see
        // it, and before v2 gets a `Move` — the key's buffered `Typed` would
        // otherwise be replayed at the hop's landing and lay the very cell
        // the classifier refused. The classifier has already forgotten the
        // presses and spent the stamp; the caller treats the move as any
        // declined one (v2's mirror follows it only across a row).
        if mv.landing_foreign {
            self.last_licensed_row = licensed_row_before;
            self.log_decline(now, (pr, pc), (cr, cc), Self::DECLINE_PROGRAM_ROW);
            return false;
        }
        // A FRESH stamp is a LICENSE (v0.43.0 law): the resize/Enter gesture
        // behind a move earns the ZOOM/starburst arm even from a cold momentum
        // spine (the `disp >= RAINBOW_JUMP_MIN_DISP || return_licensed ||
        // reflow_licensed` disjunction in `spawn_move_light`). Hard-wiring
        // these false made a cold Enter land dark forever — the streak the
        // owner had at v0.43.0.
        // THE RESIZE LICENSE, consumed ONCE per spawn for EVERY style (owner,
        // 2026-07-28: "all get"): `spawn` runs only on an observed move, and
        // the first licensed move after a settled resize IS the relayout
        // relocation, so this pairs with the right one. It also suppresses the
        // landing payload — a resize gets the BEAM ONLY.
        // ORDER: the retirement arms PEEK the Return/newline classifiers
        // ("Peeked, never consumed as retirement morphology" — the Enter
        // exemption and the Enter-exhale carve-out both say so), so
        // `retire_abandoned_light` must run BEFORE this spawn consumes its
        // one-shot licenses. With the takes hoisted above it, both peeks read
        // a hint that was already spent on this very move and the documented
        // exemptions were structurally dead — the Enter clamp fired through
        // its own carve-out.
        let gap = self.update_typing_thermals(&mv);
        self.retire_abandoned_light(&mv);
        let reflow_licensed =
            take_hint_fresh(&mut self.reflow_hint, now, Self::REFLOW_HINT_FRESH).is_some();
        // Consume Return once; a FRESH stamp is the bare-Enter license for the
        // jump arm ("a cold Enter throws the streak again"), and a stale one
        // must not survive as morphology for a later move.
        // …NOT by a glyph's echo (Rainbow Kitty): a same-row FORWARD hop
        // with a typed pair — a fresh stamp's `+1`, a coalesced batch, the
        // in-flight pool's drain — is a key's echo and never the Return's
        // move (a Return goes down, or back to the prompt). `gi⏎` typed
        // inside the window with `g`'s echo in flight (Claude Code's
        // measured input p99 is 268 ms) would take the Return on `g`'s
        // `+1`, class it `Return`, and the forget edge would drop `i`'s
        // credit: `i`'s echo, its stamp fresh and no press unpaid, refused
        // dark. The Return (and the composer newline, peeked in the licence
        // mapping) stays for its own row change and is spent there; §17.3:
        // "the in-flight echoes that precede a Return's own move are still
        // licensed by their credits". The classic styles keep the
        // class-blind take: their trail twin consumes its generic move hint
        // on every spawn, and the two engines license in lockstep. The
        // shape is a fresh stamp WITH A PRESS IN FLIGHT: `typed_hinted`
        // alone is a stamp, and a coalesced echo leaves K−1 of them
        // dangling with nothing left to pay for — keyed on the stamp alone
        // the Return would survive every keyless same-row +1 on a dangling
        // stamp, the seam sweeping a program cell per stamp as `licensed
        // key` (the law `seam_licensed` holds: a typed stamp licenses light
        // only while its press is unpaid). The pool is read before the
        // classifier spends it (`unpaid_before`): after a coalesced echo it
        // is empty on the very move the exemption serves.
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
        // …and the composer newline is consumed the same way, once: peeked
        // and never taken, one Shift+Enter would license every move for
        // 250 ms — a keyless +1 flying a `Licence::Return` meteor, a
        // keyless relocation drawing a jump.
        let newline_taken = if glyph_echo {
            None
        } else {
            take_hint_fresh(&mut self.newline_hint, now, Self::RETURN_HINT_FRESH)
        };
        let newline_licensed = newline_taken.is_some();
        // Consume the Tab / scripted gesture once even though the timestamp
        // cannot prove which later PTY movement, if any, the child authored.
        // (Class-blind, as always: a typed echo licensed by its own stamp
        // takes it too. A Tab's delivered-insert stamp, armed at the same
        // instant, is spent with it only when the gesture class itself
        // LICENSED the move — see the v2 licence mapping below.)
        let gesture_taken = self.user_gesture_hint.take();
        self.cue_move_sound(&mv);
        // THE KILL'S RETREAT — one kill, one retreat: a ^U / ^W's caret
        // retreat is licensed by the one retreat it is owed
        // ([`Self::kill_retreat_pending`], bounded by its OWN stamp —
        // `kill_hint` is also the poof's row-content proof, which the probe
        // fence after a band move or a scroll may clear without the retreat
        // changing class) and spent here, where every style reads the
        // licence: the retreat is a property of the MOVE, not of the
        // renderer, and `note_motion` supersedes it either way.
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
        // SEAM POINT 1 (§17.2, D14): the observed, LICENSED move goes to v2
        // AFTER the licence gate, `classify_move` (which consumed the nav /
        // typed hints), the thermals and the one-shot licence takes above —
        // and v1 lays NOTHING for it: not a spark, a ZOOM, a starburst, a
        // ring, a shower, a glide sample or a hue step, or two ribbons draw.
        if self.v2.engaged() {
            self.hand_v2_the_move(&mv, licence, kill_retreat_at, call);
            return true;
        }
        let boost = self.birth_boost(&mv);
        let spark_life = self.move_spark_life(&mv, gap);
        // Advance each style's ancillary rolling hue clock. Coalesced runs take
        // one step per swept cell so spawn metadata is cadence-independent.
        // RainbowKitty's visible ribbon and streak colours do not read this
        // clock; they are assigned by `reflow_classic_run` below.
        let hue_step = match cfg.style {
            GlowStyle::Phaser => RAINBOW_PHASER_HUE_STEP,
            _ => RAINBOW_ROLL_HUE_STEP,
        };
        // FADE AS ONE: a spark's chained life is a BET that the rhythm that
        // produced it continues. Under a live chain (Phaser / Beam) every
        // typing spark's death is re-priced to the LAST key's cadence, so a
        // run fades together instead of tail-first.
        if mv.typing
            && matches!(cfg.style, GlowStyle::Phaser | GlowStyle::Beam)
            && gap <= Self::PHASER_CHAIN_GAP_MAX
        {
            for s in self.sparks.iter_mut().filter(|s| s.typing) {
                let age = now.saturating_duration_since(s.born).as_secs_f32();
                s.life = s.life.min(age + spark_life);
            }
        }
        // FIRE METEOR classification for a licensed non-typing move: a row
        // change or long same-row skip flies one pixel-space streak along the
        // true vector and spawns no per-cell sparks.
        let fire_meteor =
            mv.fire && !mv.typing && (mv.dr_abs >= 1 || mv.dc_abs >= Self::METEOR_MIN_COLS);
        // The DIAGNOSIS census, read once before and once after the emitters so
        // `ctl trail` can say whether a licensed move actually put anything on
        // glass. Eight `len()` reads on the spawn edge — never the frame path.
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
        // Advance the rolling rainbow hue a little each move. Phaser cycles at
        // 2× so each colour holds for a shorter stretch of typing — the fat
        // beam visibly sweeps the spectrum instead of dwelling on one hue. A
        // coalesced typing run advances one step per LAID CELL (hue_advances),
        // so the spectrum's spatial period is the same at any echo cadence.
        self.hue = (self.hue + hue_step * hue_advances).fract();
        self.spawn_lightning(&mv);
        self.spawn_landing_ring(&mv, fire_meteor);
        self.spawn_burst_particles(&mv, fire_meteor);
        self.spawn_pack_particles(&mv, fire_meteor);
        if self.particles.len() > Self::MAX_PARTICLES {
            let drop = self.particles.len() - Self::MAX_PARTICLES;
            self.particles.drain(0..drop);
        }
        // The move was licensed and its light is laid: score it for
        // `trail status` (see [`Self::spawns`]). One `u64` add on the spawn
        // edge — not the frame path — so an idle or non-moving cursor pays
        // nothing at all.
        self.spawns += 1;
        if self.born_geometry_census() == born_before {
            // Licensed, classified — and the style's shape gates minted
            // nothing (a re-anchor that lays only its landing cell, a ZOOM
            // that declined, a Trail Pack with no matching arm). The ring says
            // which of the two dark causes it was; `no-credits` names the one
            // gate the owner asked for by name.
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

    /// Record a licensed TYPED-class verdict: `licence=key` for a press-hint
    /// class, `licence=inflight` for a move the in-flight pool alone licensed,
    /// which is also tallied for `trail status`.
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

    /// How much BORN geometry the engine holds right now — the census the
    /// spawn seam brackets its emitters with so the diagnosis ring can tell a
    /// licensed move that painted from one that fell through every shape gate.
    /// Reads only; no allocation.
    pub(super) fn born_geometry_census(&self) -> usize {
        self.sparks.len()
            + self.particles.len()
            + self.fire_meteors.len()
            + self.bolts.len()
            + usize::from(self.ring.is_some())
    }

    /// Classify one LICENSED cursor move — typing vs. jump, with the
    /// wrap/re-anchor/coalesced-echo collapses and bounded classifier hints —
    /// and feed the fast-glide velocity spine. The license upstream has already
    /// established that a human touched the keyboard; this decides WHICH
    /// choreography that press earns.
    #[allow(
        clippy::too_many_arguments,
        reason = "from/to cursor cells + clock + config + geometry + lane + the call; packing them into a struct would only obscure a single internal call site"
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
    ) -> MoveCtx<'a> {
        // A flushed park whose source row the content witness carried away
        // (`HeldPark::content_followed`): the proof a short wrap retreat
        // needs below.
        let content_followed = matches!(
            call,
            SpawnCall::FlushedPark {
                content_followed: true,
                ..
            }
        );
        // A single-cell advance is TYPING; a multi-cell delta is a real cursor JUMP.
        // The jump distance drives BOTH the comet lifetime and the ring/particle burst.
        let dr_abs = (cr as i32 - pr as i32).abs();
        let dc_abs = (cc as i32 - pc as i32).abs();
        let raw_dist = dr_abs.max(dc_abs) as f32;
        // A same-row FORWARD hop — the one shape a key's echo takes.
        let fwd = cr == pr && cc > pc;
        // A typing WRAP — the cursor spilling from the last column to the START of the
        // next row because a typed glyph filled the line — is CONTINUED TYPING, not a
        // jump. Its raw delta is large and diagonal (right edge → col 0 one row down), so
        // without this EVERY wrap during fast typing erupts an inter-line meteor / beam
        // that sweeps ACROSS the whole line — landing fire on the prompt and other cells
        // the cursor never typed (the owner's "fire on wrong cells during heavy typing").
        // Detected by SHAPE — down exactly one row, landing at the left edge, from at/near
        // the last column (a deferred-wrap echo lands at col 1; a wide glyph wraps from the
        // second-to-last column) — then its distance is COLLAPSED to one cell so every
        // jump-vs-typing decision below (meteor, bolt, ring, particle burst, swept line)
        // treats a wrap exactly like typing the next glyph: just the wake at the new cell.
        // The second arm catches the COALESCED wrap echo (a fast burst delivers 2-3
        // glyphs across the fold in one observed move, landing at col 2-3 from within
        // four columns of the edge). Three guards keep it honest: `cc >= 1` — at
        // least one glyph already landed on the new row, so a plain Enter (col 0)
        // can only match the original tight arm; a LIVE typing rhythm (`last_type`
        // within the chain window) — mid-burst only, so a cold-start jump that
        // happens to match the shape keeps its owner-mandated drama; and the MAIN
        // screen — on the alt screen the blink discriminator below selects
        // re-anchor morphology only after candidate admission.
        let wrap_echo_rhythm = self.last_type.is_some_and(|t| {
            now.saturating_duration_since(t).as_secs_f32() <= Self::PHASER_CHAIN_GAP_MAX
        });
        let (pane_col0, pane_cols) = self.pane_span(geom);
        // THE LICENCE, before the fold shapes: the
        // coalesced fold's second witness is the in-flight licence, so the
        // licence is decided first. PEEKED, not yet consumed: whether the
        // stamp is spent is decided once the shape says whose echo this is
        // (see `older_batch` below).
        let fresh_stamp = self.type_hint.peek_fresh(now, Self::TYPE_HINT_FRESH);
        let mut typed_at = fresh_stamp;
        let mut in_flight_licence = false;
        // **AN UNPAID PRESS IS ITSELF A TYPED LICENCE.** The stamp bank asks
        // how long ago the last KEY was; a debounced app repaints when it
        // likes, and half a second after the hand paused is routine —
        // measured at 5 keys/s into a 1200 ms debounce, every hop painted
        // except the last, which declined `no-fresh-hint` and left four
        // cells background-black AT THE CARET.
        //
        // So when no stamp is fresh, ask the LEDGER instead: is there a press
        // whose glyph has not been laid? That is the whole content of "a typed
        // echo", it is the same law the credit ring was fixed to state, and it
        // is self-limiting — program output banks no credits, so a keyless
        // caret walk finds an empty pool and is refused exactly as before
        // (`a_caret_that_advances_with_no_unpaid_press_still_buys_nothing`).
        // Gated to the ECHO SHAPE, a same-row forward advance or a paid fold;
        // every other shape keeps the stamp window byte-for-byte. The
        // licence's clock is the OLDEST unpaid press, matching `take_fresh`'s
        // press order and the rule that a swept cell is born at its KEY's
        // clock, not its echo's.
        if typed_at.is_none() && self.unpaid_typed_echo(pr, pc, cr, cc, now, cfg, geom) {
            typed_at = self.type_press_ring.oldest_unpaid(now);
            in_flight_licence = typed_at.is_some();
        }
        // The tight arm is [`Self::fold_shape`] and the coalesced arm
        // [`Self::coalesced_fold_shape`] — one predicate each for the
        // classifier and the in-flight seam. The coalesced arm needs a
        // witness beyond its shape: a live typing rhythm (mid-burst only,
        // so a cold-start jump that happens to match keeps its
        // owner-mandated drama) or the in-flight licence — the pool paid
        // for it exactly, and a stall ages the rhythm out.
        let tight_fold = self.fold_shape(pr, pc, cr, cc, geom);
        let coalesced_fold = self
            .coalesced_fold_shape(pr, pc, cr, cc, now, geom)
            .filter(|_| wrap_echo_rhythm || in_flight_licence);
        let shape_wrap = tight_fold.is_some() || coalesced_fold.is_some();
        // The cells a fold LAYS — the origin row's tail plus the landing
        // row's head, the shape's own count (so the spend agrees with the
        // in-flight seam: after a paid pending wrap the tail is 0, see
        // `fold_origin_tail`) — which is what the fold spends, so a batch's
        // tail behind it survives for the next row's keys.
        let fold_cells = tight_fold.or(coalesced_fold);
        // TYPED RE-ANCHOR: after exact/synthetic admission, a fresh typed or
        // Backspace classifier can collapse a one-row move beyond the typed
        // advance into TUI repaint morphology. The caret never travelled through
        // the interpolated cells, so it must not meteor/ZOOM/bolt/sweep them.
        // `dr_abs <= 1` covers wrap-down (typed glyph), wrap-back-up
        // (backspace joining the previous visual line), AND the BOX-GROWTH wrap
        // (live-verified in Claude Code: its bottom-anchored input box grows a
        // row UP when the text wraps, so the caret's TERMINAL row stays
        // constant — the observed move is dr == 0 with a huge column delta; no
        // scrollback moves on the alt screen, so the scroll translation cannot
        // catch it either). Multi-row typed-paired moves (vim gg/G/{/}) stay on
        // the owner-mandated meteor path;
        // `raw_dist > 2.0` keeps every unproved ConPTY hide-bridged move
        // (chebyshev ≤ HIDE_BRIDGE_MAX_DIST = 2) byte-identical. Only an
        // exact held park whose row content followed may use the short arm.
        // The typed classifier is consumed once (one hint, one echo) — below,
        // after the spend. Peek the quench classifier because the deletion
        // arm below owns its consumption.
        let typed_pair = typed_at.is_some();
        let bs_pair = hint_fresh(self.quench_hint, now, Self::QUENCH_HINT_FRESH);
        // A BACKSPACE'S ECHO IS A RETREAT; IT CAN NEVER EXPLAIN A SAME-ROW
        // FORWARD HOP. The fresh quench hint vetoes the typed arms below so
        // a Backspace's own landing is never laid as typing — but a FORWARD
        // hop inside its window is the survivors' echo (`abc⌫` draining as
        // `+2` sixty milliseconds after the key, or `ab⌫c` inside one frame
        // gap): with the veto applied there the hop is licensed yet lays
        // nothing, spends nothing and sweeps nothing, its cells dark and
        // its credits a phantom pool for the whole patience. The veto keeps
        // every backward and cross-row
        // shape (the Backspace's retreat, its wrap-up re-anchor) and lifts
        // for the forward one, which is the same classification the hop
        // gets one quench window later.
        let bs_fwd = bs_pair && !fwd;
        // A RETURN'S RESPONSE IS NEVER THE GLYPH'S RE-ANCHOR: with the
        // Return one-shot surviving a typed press, the Enter's late
        // response (down a row, or back to the prompt on the composer's
        // row) arrives beside the type-ahead glyph's fresh stamp. It is
        // the Return's move — licensed and spent as such at the seam — so
        // it neither re-anchors on the stamp (which would lay the landing
        // cell LEFT of the new caret: the prompt's cell, lit with no key
        // behind it) nor pops the stamp, which the glyph's own echo still
        // needs. A same-row FORWARD hop is excluded exactly as
        // `glyph_echo` excludes it: that shape is a key's echo, never a
        // Return's.
        let return_paired = !fwd && hint_fresh(self.return_hint, now, Self::RETURN_HINT_FRESH);
        // …and the COMPOSER NEWLINE's row change is paired the same way:
        // with the newline surviving a typed press, Shift+Enter's late line
        // break arrives beside the
        // type-ahead glyph's stamp and is the newline's move, never the
        // glyph's re-anchor. Unlike the Return, the chord banked a typed
        // stamp of its OWN (the host's Shift+Enter arm), and the break is
        // that stamp's echo: the take below pops the bank's oldest fresh
        // stamp — the chord's — and leaves the glyph's for its own echo.
        let newline_paired = !fwd && hint_fresh(self.newline_hint, now, Self::RETURN_HINT_FRESH);
        // ALT-SCREEN blink classifier: the repaint blink distinguishes a
        // full-redraw TUI re-anchor from a deliberate vim/less jump. On the
        // main screen the conjunct is vacuously true.
        let blink_fresh = self.blink_fresh(now);
        // A fresh NAV timestamp VETOES a dying typed re-anchor class: Ctrl-A/E,
        // Home/End are deliberate jumps whose meteor is owner-mandated, and a
        // same-row nav leap right after typing would otherwise pair with the
        // dying typed hint (peeked — the navigation classifier below owns the
        // consumption).
        // …but NOT a same-row FORWARD hop the in-flight pool pays for
        // (Rainbow Kitty): that is a glyph's echo landing after the arrow's
        // press — `abcd⇥End` over a 300 ms link, an autosuggestion accepted
        // right after typing, a stalled app draining frame-split — and
        // never the arrow's move, which goes wherever the line ends. Judged
        // as the arrow's it would spend the nav one-shot, the forget edge
        // would drop the pool, and the rest of the row's echoes would be
        // refused dark, the End hop with them. The Nav twin of
        // `glyph_echo` (the Return's): one unpaid press
        // for one cell, the coalesce's own share rule (two presses, three
        // quarters of the cells) for a batch. The hint is LEFT for the
        // arrow's own hop; an unpaid or under-paid forward hop still reads
        // as the arrow's, and the classic styles keep the class-blind take
        // (their trail twin consumes its nav hint on every spawn).
        // A MOVING KILL's own echo is a RETREAT, never a forward hop: a
        // forward hop inside its window is the glyphs it is about to erase
        // landing late (`ab⌃W` over a slow link) — the kill forgot their
        // pool at the key, so it flies as `Synthetic` (a sub-floor nav
        // tick) rather than as the kill's retreat, and the retreat that
        // follows keeps the nav stamp, its `Kill` and its drain.
        let glyph_fwd = matches!(cfg.style, GlowStyle::RainbowKitty)
            && fwd
            && (hint_fresh(self.kill_retreat_pending, now, Self::KILL_HINT_FRESH)
                || (dc_abs == 1 && self.typed_credits_within(now) >= 1)
                || (dc_abs >= 2 && self.typed_share_paid(cr, pc, cc, now)));
        let nav_paired = !glyph_fwd && hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH);
        // ECHO RUN — TYPING CONTINUATION: a fast burst's echo can advance the
        // cursor 2-3 cells between two observed frames (coalesced PTY echo — at
        // robotic cadence routinely, at human cadence whenever a frame slips).
        // Paired with a fresh typed classifier and admitted candidate it is
        // CONTINUED TYPING, not a jump:
        // classifying it as a jump slams the flare, fires the ring, and lays
        // long-lived jump sparks between short-lived typing sparks — a "picket
        // fence" burst trail. The swept cells are laid as
        // per-key typing sparks below, so no typed cell is skipped. RAINBOW KITTY is
        // EXCLUDED — its own `rainbow_coalesce` classifier (below) owns the
        // rainbow coalesce, so a 2-3 cell rainbow echo is never double-classified,
        // and the `!echo_run` re-anchor veto stays a phaser/non-rainbow-kitty concern
        // (rainbow-kitty re-anchors exactly as origin intended, its path prioritising
        // `rainbow_coalesce`'s swept cells).
        // Reach: the SHARED typed-bridge law (8 cells — `HIDE_BRIDGE_TYPED_MAX_DIST`),
        // not a private 3. A 4-8 cell typed advance in one observed frame is routine
        // under coalesced echo (key-repeat across a frame slip, SSH batching); a
        // tighter cap drops those into `re_anchor`, which lays ONLY the landing cell —
        // a multi-cell dark notch splitting the band.
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
        let wrap = shape_wrap || re_anchor;
        // THE TYPED KEY'S MOVE under Rainbow Kitty: a typed-paired move no
        // Backspace or nav one-shot owns; and THE TYPED FORWARD HOP, its
        // same-row forward shape, which on the alt screen keeps the blink
        // discriminator (only an admitted repaint-blinking app coalesces).
        // Every typed arm below is one of these two plus its own width
        // clause, so the claims they make of each other are visible.
        let typed_key =
            matches!(cfg.style, GlowStyle::RainbowKitty) && typed_pair && !bs_fwd && !nav_paired;
        let typed_fwd = typed_key && fwd && (!self.ctx_alt || blink_fresh);
        // RAINBOW KITTY TYPED-COALESCE: a same-row FORWARD hop of 2..=RAINBOW_TYPED_SWEEP_MAX
        // cells backed by an admitted typed candidate is CONTINUED TYPING observed late —
        // PTY batching / frame quantization delivers several echoed glyphs as one
        // observed move — not a jump. It collapses `dist` exactly like a wrap, so
        // it classifies as typing (the swept cells laid by `row_sweep_cells`
        // below join the ribbon's chained-life system instead of leaving a hole)
        // AND no jump choreography — landing ring, particle burst — fires on a
        // move that is just letters arriving. Other styles keep their shipped
        // classification byte-identically. PRESS BUDGET (anti-stray): N swept
        // cells must be backed by the presses that typed them under the share
        // rule ([`Self::typed_share_paid`]) — batched echoes really deliver N
        // cells from real presses (a wide glyph's press is worth its 2 cells,
        // an IME commit its summed cells — the host prices them at the input
        // boundary), while vim's normal-mode `w` is ONE credit echoing as a
        // multi-cell hop (and its statusline repaint can read as blink_fresh,
        // defeating the alt-screen discriminator alone). One credit must
        // never paint a ribbon over a word the user only skimmed.
        // `typed_credits_within` counts UNSPENT credits — presses whose
        // glyphs have not been laid — for the in-flight patience, not
        // whatever fell inside a window measured from the observation
        // ([`PressCredits`]: the ledger, not the clock).
        let rainbow_coalesce = typed_fwd
            && dc_abs >= 2
            && dc_abs as usize <= Self::RAINBOW_TYPED_SWEEP_MAX
            && self.typed_share_paid(cr, pc, cc, now);
        // The one shape the CREDIT BUDGET, and only the credit budget, refused:
        // everything else about this move said "coalesced typing" and the
        // presses to pay for the swept cells were not there. Recorded for the
        // diagnosis ring (`no-credits`) so `ctl trail` can tell a budget refusal
        // apart from a shape that simply is not typing.
        let credit_starved = typed_fwd
            && !rainbow_coalesce
            && dc_abs >= 2
            && dc_abs as usize <= Self::RAINBOW_TYPED_SWEEP_MAX;
        // The same shape PAST the cap: a re-anchor that lays only its
        // landing — and a forget edge, because the presses in flight cannot
        // describe a hop the cap refuses.
        let typed_over_cap = typed_fwd && dc_abs as usize > Self::RAINBOW_TYPED_SWEEP_MAX;
        // **THE BATCH BEHIND THE STAMP** (the key typed as the stalled
        // frame lands). A coalesce whose cells are WHOLLY covered by
        // presses OLDER than the fresh stamp is the older presses' batch —
        // the stamp's own key was pressed after the app had already queued
        // the batch, and its glyph is not in this hop. So the sweep is dated
        // at the OLDEST unpaid press (the ledger partitions by that clock:
        // the older presses are the sweep's own, nothing lies to its left,
        // and the stamp's press stays banked on the ledger for its own echo),
        // and the stamp is LEFT in the bank for the echo that is its own — a
        // frame later. Popping it here dated the sweep at the fresh key, the
        // ledger forfeited every press (thirty older than the key, no hole),
        // and the key's own echo then found no stamp and one credit: one dark
        // cell after every stall recovery the hand typed through.
        let older_batch = rainbow_coalesce
            && fresh_stamp.is_some_and(|stamp| {
                self.type_press_ring.cells_where(now, |t| t < stamp) >= dc_abs as usize
            });
        let typed_at = if older_batch {
            self.type_press_ring.oldest_unpaid(now).or(typed_at)
        } else {
            typed_at
        };
        // **A CREDIT IS SPENT BY THE CELLS IT LAYS.** Every forward same-row
        // typed echo spends, not only a coalesced one — an ordinary 1-cell
        // echo is a press whose glyph has just landed, and a press whose
        // glyph has landed must not be able to buy a second cell somewhere
        // else. This is the whole discrimination a window measured from
        // the observation cannot make, in BOTH directions: it refuses a
        // real batched echo because the presses that produced it have aged
        // out (the owner's black gaps), and it ADMITS vim's `w` whenever
        // five ordinary letters happened to be typed inside the preceding
        // half second, their credits still in the ring with their cells
        // already on glass. Draining the pool as the cells land fixes both
        // at once: what is left in the ring is exactly the light that has
        // been paid for and not yet delivered.
        //
        // Spend only what was there: the share rule can admit a sweep whose cells
        // outnumber its credits, and the pool must not go negative or wrap.
        // The FOLD lays its cells too (`lay` walks back across the pane wrap), so
        // its presses are paid and must leave the pool — otherwise a wrap's
        // credits sit unspent and fund a later keyless hop on the new row, which
        // is exactly what `rainbow_typing_wrap_folds_around_the_line_end` refutes.
        let lays_typed_cells =
            typed_key && ((fwd && (rainbow_coalesce || dc_abs == 1)) || shape_wrap);
        // **A TYPED RE-ANCHOR SPENDS ITS LANDING.** A typed-paired
        // re-anchor — the box-growth wrap, a flushed park, a fresh-stamp
        // `w` — consumes the stamp below and lays exactly ONE cell, the
        // landing; spending nothing, the stamp's own credit would sit in
        // the pool and license the next KEYLESS +1 on the row inside the
        // patience as `inflight`: light no keystroke asked for. A credit is
        // spent by the cells it lays — one here, with the stamp. (The
        // forward re-anchors past the cap or the share rule are forget
        // edges below; the spend before them is moot.)
        // …and lays it only while a press is unpaid: the spend below is
        // then non-zero by construction, and the landing sweep at the seam
        // is gated on the same predicate, so a dangling stamp cannot light
        // a keyless hop's landing.
        // …and ordinarily only on a KEY's stamp, never on the in-flight
        // pool alone (the paste harness's flood control): the landing is
        // v1's law for a hop a press licensed inside the stamp window
        // (vim's `w`, the box-growth re-anchor, a flushed park). The one
        // exception is a delayed composer fold whose old source tail is
        // visibly relocated after the exact unpaid glyph on the next row
        // (`key_pushed_text_down`). That content identity makes its landing
        // a cell the key laid even after the stamp's 250 ms has passed.
        // The in-flight licence opens for ANY same-row forward hop while
        // two presses are unpaid (`unpaid_typed_echo` — a batch, judged
        // under the share rule), and the only re-anchors that reach here
        // on it are the hops the share rule or the cap REFUSED — forget
        // edges, "the presses in flight cannot describe" them. Two
        // presses whose echoes were never judged (a hidden-caret TUI's
        // first key on a row the anchored lane had never seen) sit on the
        // ring for the whole patience, and a twelve-cell program flood with
        // no key behind it would spend one of them on its last cell. A hop
        // no stamp licensed and no pool describes is program output: dark.
        let re_anchor_landing = typed_key
            && (!in_flight_licence || self.carried_key_move == Some(((pr, pc), (cr, cc))))
            && re_anchor
            && !lays_typed_cells
            && self.typed_credits_within(now) >= 1;
        // **THE LANDING IS THE KEY'S OWN GLYPH** (zsh's Ctrl-R, 2026-09-23 —
        // the match that HOPS). The landing law exists for the hop whose
        // landing cell IS the typed glyph: the box-growth wrap, a flushed
        // park, a coalesce the share rule refused whose last glyph is the
        // key's. zle's incremental search is its counter-shape: it echoes
        // each key on the status row below and walks the VISIBLE caret along
        // the match row to the newest entry's rightmost occurrence — `ls
        // -la` on `l` hops four cells to its second `l`, `echo hello world`
        // on `lo` four more, one credit behind each — so the landing sat
        // under `-` for the `l`, under `hello`'s first `l` for the `o`, and
        // the ribbon walked the cells between them: a band on history text
        // nobody typed. So on the VISIBLE lane (the anchored lane's parked
        // echo has its own gate, in [`Self::echo_anchor_pass`]) the landing
        // is laid only where THIS frame's caret-row probe
        // ([`Self::probe_glyph_before`]) holds a live press's own glyph. A
        // landing foreign to every live press forgets the presses instead of
        // spending one — their echo went elsewhere (zle's minibuffer), and
        // left banked the `l` would match `hello`'s `l` under the next hop —
        // and [`Self::spawn_judged`] declines the move `program-row`. Unknown
        // — no probe of the row on this frame (a held park, judged at its
        // own clock, reads the glyph it was held over instead — below), a
        // glyph-less press (a wide glyph, an IME commit, a count-only host),
        // an empty pool — keeps the landing law exactly as it was.
        //
        // **THE WITNESS READS THE CELL THE RE-ANCHOR LAYS** (fix/trail-land:
        // the zsh gate above meets Claude Code 2.1.280's re-wraps). Two
        // composer verdicts move that cell off `(cr, cc − 1)`, and each is
        // read FIRST so the gate judges the cell actually laid, never one the
        // move deliberately leaves dark:
        // * a SOFT-WRAPPED CARET ([`Self::soft_wrapped_caret`]) lays its one
        //   cell at the ORIGIN `(pr, pc)`, where the key's glyph stands; its
        //   `cc − 1` is the continuation row's blank indent, foreign to every
        //   press but a Space, and read there the gate refused every wrap key
        //   judged after a pause (`claude_wrap_band.rs`'s PauseBeforeWrap:
        //   the key declined `program-row`, its presses forgotten, the word's
        //   first letter dark on row 2). So the witness is the origin's glyph
        //   in the probed row above — a key there no live press typed is
        //   still refused, so zle cannot pass a status-row glyph off as a
        //   soft wrap;
        // * a LIFTED WORD ([`Self::lifted_word`]) lays NO landing (the seam's
        //   landing sweep stands down on `lift`): its landing is the word's
        //   old first column, and left of it stands the continuation row's
        //   indent (or a box border), which no key of the lift wrote. There
        //   is no laid cell to witness, so the gate stands down with the
        //   sweep and the move reaches v2 with its `Lift`. The lift's own
        //   probe evidence (a whole word gone from this row and arrived at
        //   the END of the row above, blank there before) is no zle shape:
        //   zle's hops rewrite the match row and never the row above it.
        // The typed fold (`lays_typed_cells`) never reaches the gate, and the
        // other re-wrap verdicts need no exemption: a key that pushed the
        // row's text down ([`Self::key_pushed_text_down`]) and the echo that
        // reflows a soft-wrapped word ([`Self::carried_word_left`]) both land
        // right of the key's own glyph, which the gate admits.
        let soft_wrap = if lays_typed_cells || re_anchor_landing {
            self.soft_wrapped_caret(pr, pc, cr, cc, pane_col0, now)
        } else {
            None
        };
        let lift = if typed_key && cr == pr && cc < pc {
            self.lifted_word(pr, pc, cr, cc)
        } else {
            None
        };
        let laid_cell_end = match (soft_wrap, lift) {
            (_, Some(_)) => None,
            (Some(_), None) => pc.checked_add(1).map(|end| (pr, end)),
            (None, None) => Some((cr, cc)),
        };
        // **A FLUSHED PARK IS WITNESSED BY THE GLYPH IT WAS HELD OVER**
        // (fix/trail-land review: zsh's match walking BACK along its row —
        // `l` puts the caret on `world`'s `l`, `lo` retreats it to `hello`'s
        // `lo`). A same-row retreat with a press in flight is held as Ink's
        // park ([`Self::park_candidate`]) and flushed at its own clock, where
        // no probe is that frame's: read there, the witness was unknown and
        // the re-anchor laid its landing under `hello`'s first `l`. So the
        // flush reads the glyph the landing cell held when the park was held
        // ([`HeldPark::landing_cell`]), against the presses live at the
        // park's clock (the later ones are set aside for the judgment, so a
        // refusal forgets only the presses banked by then). Only a GLYPH
        // there is judged: a blank landing already lays nothing
        // ([`HeldPark::landing_glyph`]), and Ink's park to the input's start,
        // beside the prompt's blank, keeps the verdict and the presses it
        // had.
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
        if lays_typed_cells || re_anchor_lays_landing {
            // …and a SOFT-WRAPPED CARET's fold or re-anchor lays exactly
            // one cell too: the key's own glyph, at the origin
            // ([`Self::soft_wrapped_caret`]). The fold's shape counts the
            // origin row's tail to the pane's edge and the landing row's
            // head, where only `pc` holds a glyph; spending the rest would
            // spend presses whose glyphs are still to come.
            let credits = self.typed_credits_within(now);
            let laid = if re_anchor_lays_landing || soft_wrap.is_some() {
                1
            } else {
                fold_cells.unwrap_or(dc_abs as usize)
            };
            let cells = laid.min(credits);
            if fold_cells.is_some() {
                fold_laid = cells;
            }
            // SPENT BY THE CELLS IT LAYS, oldest-first: the presses that
            // produced this echo are the ones consumed, so one pool of real
            // typing can never fund a SECOND multi-cell echo (a scroll-by
            // that happens to land inside the window buys nothing — its
            // cells were already paid to the move they belong to).
            let _ = self.type_press_ring.spend_counting(now, cells);
        }
        // Consume the typed classifier once (one hint, one echo) — unless
        // this hop was the older presses' batch AND the stamp's own press is
        // still unpaid after the spend (a stamp re-banked at a paste's
        // delivery sits after its own press; when the spend took that press
        // the stamp goes with it, exactly as before) — and never on a
        // one-shot's OWN move: the Return's response or
        // the Backspace's retreat landing after a type-ahead glyph is not
        // that glyph's echo, and the stamp is left for the echo that is.
        if let Some(stamp) = fresh_stamp
            && !(return_paired || bs_fwd)
        {
            let own_press_unpaid =
                older_batch && self.type_press_ring.cells_where(now, |t| t >= stamp) >= 1;
            if !own_press_unpaid {
                let _ = self.type_hint.take_fresh(now, Self::TYPE_HINT_FRESH);
            }
        }
        // Honour BOTH coalesce paths: `rainbow_coalesce` collapses a late-observed
        // RAINBOW KITTY rainbow echo, `echo_run` the same for PHASER/non-rainbow-kitty (they are
        // mutually exclusive by style — `echo_run` excludes rainbow kitty), and `wrap`
        // the fold/re-anchor. Any of the three classifies the move as typing.
        let dist = if wrap || rainbow_coalesce || echo_run {
            1.0
        } else {
            raw_dist
        };
        let typing = dist <= 1.0;
        let fire = matches!(cfg.style, GlowStyle::Fire);
        // EMBERFORGE momentum direction: only a FORWARD advance (one column
        // right on the same row — a typed glyph's echo) earns heat/coal.
        // Backward, vertical, and diagonal single-cell moves still spawn their
        // wake (visual continuity) but add no momentum — scrubbing around a
        // file is navigation, not writing. A DELETION echo is the move paired
        // with a fresh Backspace key-hint ([`Self::note_backspace`]): it also
        // earns nothing, and consuming the hint here keeps hint state bounded.
        let forward = typing && cr == pr && cc as i32 == pc as i32 + 1;
        // A same-row FORWARD hop is never the deletion (see `bs_fwd`): a
        // stalled batch draining three cells inside the window is the
        // survivors' coalesce and not a `FoldReverse` re-anchor. The quench
        // hint is CONSUMED by that drain all the same: the erase it was
        // armed for is already in the drained row, and a one-shot left
        // standing would license every keyless `+1` on the row until it
        // expired — a spinner advancing one cell at a time after a stall
        // drain lighting every step. One hint, one echo, whichever shape —
        // INCLUDING A JUMP: gated on `typing`, a Backspace whose paired
        // move was not one cell (an Ink park, a TUI re-laying its box a row
        // up, the two-row hop after a band move) would leave the hint
        // standing and every program move for the next 250 ms licensed
        // `key` — three relocations, three spawns, a jump streak under the
        // comet styles. The one-shot is spent by the move it paired with
        // whatever its shape; `typing` gates only the deletion CLASS.
        let quench_taken =
            take_hint_fresh(&mut self.quench_hint, now, Self::QUENCH_HINT_FRESH).is_some();
        let deletion = typing && quench_taken && !fwd;
        // NAVIGATION classification (responsiveness): a move paired with a
        // fresh navigation key-hint ([`Self::note_navigation`]/[`Self::note_motion`]
        // — Ctrl-A/E, Home/End, held arrows, scroll chords) is SCRUBBING, not
        // writing. It earns NO heat and never slams the arrival flare, so
        // jumping to line start/end cannot erupt a full-width white-hot blaze.
        // The hint — not move shape — is the classifier, so a typed glyph,
        // Enter or wrap (which never arm the hint) still ignite normally.
        // Consumed on any paired move to keep the state bounded, and pairing in
        // ANY direction: Up/Down/PageUp/PageDown arm the very same hint, so a
        // held vertical arrow is scrubbing exactly like a held horizontal one.
        // Do NOT add a `cr == pr` conjunct: it would consume the hint on a
        // vertical move yet classify it as typing, and held Up/Down would max
        // the heat into the 1.2x sprint overdrive.
        // Gated on the PEEK above: a glyph's echo the pool paid for
        // (`glyph_fwd`) leaves the hint standing for the arrow's own hop.
        let nav_taken = if nav_paired {
            take_hint_fresh(&mut self.nav_hint, now, Self::NAV_HINT_FRESH)
        } else {
            None
        };
        let navigation = nav_taken.is_some();
        // THE CANONICAL TYPING-MOMENTUM METRIC builds in
        // [`Self::update_typing_thermals`] on the TYPED-PAIRED half — the
        // v0.43.0 law, "earned by real typing ONLY": a non-delete printable
        // advance (forward glyph echo, typing wrap/re-anchor, or coalesced
        // multi-glyph echo) paired with the fresh key-time hint `typed_at` the
        // host arms on a real printable key. The cold-output law holds by
        // construction now: program output arms no key hint, so it is not even
        // licensed to reach this classifier
        // (`program_output_alone_builds_no_momentum` pins it), and deletes and
        // kills keep DRAINING at their own key hints
        // ([`Self::note_backspace`]/[`Self::note_kill`]). Rate normalization
        // lives inside [`TypingMomentum::advance`], so a coalesced 3-glyph
        // echo credits its one observed gap — key count never buys momentum.
        // The one residual that CANNOT be resolved at the terminal layer: a
        // printable key whose echo IS a forward move (vim `l`/`w` in normal
        // mode) is byte-indistinguishable from typed text — noted, never
        // mode-detected.
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
            landing_foreign,
            soft_wrap,
            lift,
            in_flight_licence,
            wrap,
            typed_hinted: typed_at.is_some(),
            typed_at,
            bs_pair,
            // Consumed here, once per observed move: the classifier is the one
            // place that reads it, and leaving it armed would let one scroll
            // veto every later relocation.
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
