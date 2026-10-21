// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed input custody: who typed what and what it may still pay for. The
//! typed stamps, the press-credit ledger, the insert seam, and the host's
//! `note_*` input API that feeds them.

use super::*;

/// Whether a one-shot hint stamped at `hint` is still inside its `window`
/// (seconds) at `now`: the freshness predicate every licence read uses. An
/// unstamped hint is never fresh; a stamp from the future reads as fresh.
#[inline]
pub(crate) fn hint_fresh(hint: Option<Instant>, now: Instant, window: f32) -> bool {
    hint.is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() <= window)
}

/// Spend a one-shot hint: take it only while fresh ([`hint_fresh`]), leaving a
/// stale stamp in place (one hint, one echo).
#[inline]
pub(crate) fn take_hint_fresh(
    hint: &mut Option<Instant>,
    now: Instant,
    window: f32,
) -> Option<Instant> {
    hint.take_if(|t| now.saturating_duration_since(*t).as_secs_f32() <= window)
}

/// Cap of the exact content witness for the last adjacent typed echo run. It
/// is inert while the hand rests; only a later licensed typed echo reads it,
/// so the next key may re-lay unchanged text a retired cell was typed beside.
pub(super) const RECENT_TYPED_RUN_CAP: usize = 256;

/// The farthest, in rows, a cross-row park may stand from the row it left
/// (`CursorGlow::park_candidate`): a footer repainted under a composer's box
/// while a key's echo is late. Claude Code's and agy's footers sit under the
/// box's border, two or three rows below the input
/// (`tests/codex_background_redraw.rs`). Past the adjacent row a park is held
/// only where the program's print ended (`CursorGlow::landing_ends_a_print`):
/// a bare jump of two or three rows with a press in flight (vim's `}`, `2j`,
/// `3k`) went there, and is judged on its own frame as on the adjacent-row
/// bound. A caret farther away (vim's `G`) is judged at once too, instead of
/// held for a return that never comes.
pub(super) const CROSS_ROW_PARK_ROWS_MAX: u16 = 3;

#[derive(Clone, Copy)]
pub(super) struct RecentTypedRun {
    pub(super) row: u16,
    pub(super) col0: u16,
    pub(super) len: u16,
    pub(super) glyphs: [char; RECENT_TYPED_RUN_CAP],
    pub(super) at: Instant,
    /// The walk the run was laid on (`RAINBOW-KITTY-V2.md` §43, §46): the
    /// origin of the cohort owning the run's leftmost standing cell, read on
    /// the frame that lays the run (`CursorGlow::read_fresh_run_origin`) and on
    /// each key frame that extends it. The revive hands it back with its re-lay
    /// ([`rk::ribbon::Revive`]) so the line returns on the same stops. `None`
    /// until a frame has seen a cell of the run standing.
    pub(super) origin: Option<rk::ribbon::RunOrigin>,
}

impl RecentTypedRun {
    pub(super) fn end(&self) -> u16 {
        self.col0.saturating_add(self.len)
    }
}

/// Bounded FIFO of typed-press licence stamps, packed oldest-first. K presses
/// bank K stamps (the oldest dropped past [`TYPED_STAMP_DEPTH`]) and each
/// observed echo sweep pops one, so K keys license K sweeps and program output
/// beyond them finds an empty queue; a single slot would collapse several keys
/// in one frame gap into one licence. A stamp licenses only within
/// `TYPE_HINT_FRESH` of its press and goes stale in place; under Rainbow Kitty
/// only while the press ring still owes a cell ([`CursorGlow::seam_licensed`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypedStamps {
    /// Packed oldest-first: every `Some` precedes every `None`.
    pub(super) slots: [Option<Instant>; TYPED_STAMP_DEPTH],
}

impl Default for TypedStamps {
    // Written out because `[T; N]: Default` stops at N = 32.
    fn default() -> Self {
        Self {
            slots: [None; TYPED_STAMP_DEPTH],
        }
    }
}

/// Depth of [`TypedStamps`] and [`PressCredits`], equal to the coalesced-sweep
/// cell cap ([`CursorGlow::RAINBOW_TYPED_SWEEP_MAX`]): one observed move spends
/// at most this many cells. A TUI batches by its own repaint, so one observed
/// move can carry many keys, and a press stays in flight for
/// [`IN_FLIGHT_PATIENCE_S`]; 128 covers that at a fast sustained hand and a
/// full 100-column row. Past the depth the oldest presses drop, and a deeper
/// batch is refused by the sweep cap (dark, never wrong). `pub`: the host's
/// Backspace price memory (`app_input.rs`'s `ErasePriceMemory`) is bounded by
/// it.
pub const TYPED_STAMP_DEPTH: usize = 128;

/// The in-flight patience (seconds): the upper bound on how long a typed press
/// waits for its row to echo before the clock forgets it. Every real edge
/// forgets presses sooner (a keyless backward or cross-row hop, a refused
/// forward hop, an unlicensed row change, Return, an arrow, a kill, focus
/// loss, reset); this bounds a press whose row stays silent, so it covers
/// long TUI stalls under load, not echo latency. It is twice the ribbon's
/// chain window (`ribbon::CHAIN_GAP_MAX`). [`PressCredits`] reads it by name
/// and `rainbow_kitty::ECHO_PATIENCE_S` aliases it.
pub(crate) const IN_FLIGHT_PATIENCE_S: f32 = 10.0;

/// Capacity of [`CursorGlow::anchor_rows`]: how many distinct rows' print
/// endpoints the anchored-echo lane remembers, and so how long a row keeps its
/// program-row brand ([`AnchorRow::tainted`]). A new row takes the least
/// recently printed row's slot ([`CursorGlow::remember_anchor_row`]), so a
/// status row is evicted only by sixteen other rows printing since it last
/// did, never by age. A busy Claude Code prints on at least five rows
/// (spinner, composer, bullet, two timers); losing the spinner's brand lets its
/// next digit be read as a key's echo
/// (`a_busy_tuis_status_row_keeps_its_brand_across_five_print_rows`).
pub(super) const ANCHOR_ROWS: usize = 16;

/// One entry of [`CursorGlow::anchor_rows`]: where the last print run on `row`
/// ended, plus the row-discrimination brand. `tainted` marks a program row, one
/// seen advancing while no user gesture was fresh (spinners, timers, token
/// counters). The brand is sticky for the entry's lifetime: such a row never
/// consumes a typed stamp, even when it advances inside a keystroke's
/// freshness window.
///
/// Never branded: the established echo row ([`CursorGlow::last_anchor_sweep`]),
/// which an undelivered paste may advance keylessly, and a delivered insert's
/// echo row ([`CursorGlow::insert_row_identity`]). Cleared with the entry on
/// reset or scroll.
#[derive(Clone, Copy)]
pub(super) struct AnchorRow {
    pub(super) row: u16,
    pub(super) end: u16,
    pub(super) tainted: bool,
    /// [`CursorGlow::anchor_rows_clock`] at this entry's last write: the
    /// eviction order. Stamped by [`CursorGlow::remember_anchor_row`].
    pub(super) touched: u64,
}

/// The hand's row ([`CursorGlow::hand_row`]): the row the hand types on, plus
/// the caret's screen row while a scroll or band move leaves it in doubt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HandRow {
    /// The row the last licensed typed echo landed on, carried with the
    /// content by a scroll or band move.
    pub(super) row: u16,
    /// The caret's screen row across a content move: set when a scroll or band
    /// move carried the hand's row while the hand typed at the caret. A
    /// bottom-anchored composer is redrawn on the same screen row as content
    /// scrolls above it. The caret's next sighting on either row settles it
    /// ([`Self::settle`]); a press that takes the hand off its row clears it.
    pub(super) caret_row: Option<u16>,
}

/// A generic input boundary may move input away from the drawn caret. Only
/// an observed park on that boundary's source row displaces its ownership;
/// initial program output alone establishes no such claim.
#[derive(Clone, Copy, Debug)]
pub(super) struct UnownedCaret {
    pub(super) row: u16,
    pub(super) parked: bool,
}

impl HandRow {
    /// The hand's row after a licensed typed echo landed on `row`.
    pub(super) fn at(row: u16) -> Self {
        Self {
            row,
            caret_row: None,
        }
    }

    /// Carried across a scroll or band move (`map`: an absolute row to its new
    /// row, `None` once off the grid or band). The row rides the content; when
    /// the hand typed at the caret (`at_caret`) the caret's screen row is kept
    /// beside it until the caret settles which is right. Carried off the grid,
    /// the caret's screen row is the only candidate.
    pub(super) fn carried(self, map: &impl Fn(u16) -> Option<u16>, at_caret: bool) -> Option<Self> {
        let caret_row = self.caret_row.or(at_caret.then_some(self.row));
        match map(self.row) {
            Some(row) => Some(Self { row, caret_row }),
            None => caret_row.map(Self::at),
        }
    }

    /// The caret was seen on `caret_row`: on the doubted screen row the
    /// composer stayed put and the hand's row follows the caret; on the
    /// content's row the doubt is over. Elsewhere nothing is settled.
    pub(super) fn settle(&mut self, caret_row: u16) {
        if self.caret_row == Some(caret_row) {
            *self = Self::at(caret_row);
        } else if self.row == caret_row {
            self.caret_row = None;
        }
    }
}

impl TypedStamps {
    /// Bank one press. When full, the oldest stamp is dropped — the newest
    /// [`TYPED_STAMP_DEPTH`] presses are always the ones retained.
    pub(crate) fn stamp(&mut self, now: Instant) {
        if let Some(free) = self.slots.iter().position(Option::is_none) {
            self.slots[free] = Some(now);
        } else {
            self.slots.copy_within(1.., 0);
            self.slots[TYPED_STAMP_DEPTH - 1] = Some(now);
        }
    }

    /// Wipe every banked stamp (the class-changing supersede / teardown).
    pub(crate) fn clear(&mut self) {
        self.slots = [None; TYPED_STAMP_DEPTH];
    }

    /// Take every stamp banked after `at` out of the bank, keeping the rest
    /// packed. A held park is judged at its own clock against the bank as it
    /// stood then ([`CursorGlow::flush_park`]), so a later key is not spent by
    /// its flush. Put back with [`Self::merge`]. One linear pass (this runs per
    /// key under a stall); no ordering assumed.
    pub(crate) fn split_off_after(&mut self, at: Instant) -> Self {
        let mut kept = Self::default();
        let mut later = Self::default();
        let (mut nk, mut nl) = (0, 0);
        for t in self.slots.into_iter().map_while(|s| s) {
            if t <= at {
                kept.slots[nk] = Some(t);
                nk += 1;
            } else {
                later.slots[nl] = Some(t);
                nl += 1;
            }
        }
        *self = kept;
        later
    }

    /// Put back what [`Self::split_off_after`] took, behind the remaining
    /// earlier stamps, keeping the bank packed oldest-first. The newest
    /// [`TYPED_STAMP_DEPTH`] survive exactly as re-banking through
    /// [`Self::stamp`] would leave them.
    pub(crate) fn merge(&mut self, later: Self) {
        let n_later = later
            .slots
            .iter()
            .position(Option::is_none)
            .unwrap_or(TYPED_STAMP_DEPTH);
        if n_later == 0 {
            return;
        }
        let n_kept = self
            .slots
            .iter()
            .position(Option::is_none)
            .unwrap_or(TYPED_STAMP_DEPTH);
        // The newest `TYPED_STAMP_DEPTH` stamps survive, exactly as banking
        // them one by one would leave it: the oldest kept ones go first.
        let keep = n_kept.min(TYPED_STAMP_DEPTH - n_later);
        self.slots.copy_within(n_kept - keep..n_kept, 0);
        self.slots[keep..keep + n_later].copy_from_slice(&later.slots[..n_later]);
        self.slots[keep + n_later..].fill(None);
    }

    /// Keep only the stamps `keep` admits, packed oldest-first in their
    /// banked order — the one retirement every partial clear is.
    pub(super) fn retain(&mut self, keep: impl Fn(Instant) -> bool) {
        let mut kept = [None; TYPED_STAMP_DEPTH];
        let mut n = 0;
        for t in self.slots.into_iter().map_while(|s| s) {
            if keep(t) {
                kept[n] = Some(t);
                n += 1;
            }
        }
        self.slots = kept;
    }

    /// Remove only the stamp(s) written at `at` — the revoke contract for a
    /// dispatch that never reached the child.
    pub(crate) fn revoke_at(&mut self, at: Instant) {
        self.retain(|t| t != at);
    }

    /// Whether any stamp is banked, fresh or stale.
    #[cfg(test)]
    pub(crate) fn armed(&self) -> bool {
        self.slots[0].is_some()
    }

    /// Whether any banked stamp is still within `window` of `now` — the
    /// license read (peek, never consumes).
    pub(crate) fn any_fresh(&self, now: Instant, window: f32) -> bool {
        self.slots
            .iter()
            .map_while(|s| *s)
            .any(|t| now.saturating_duration_since(t).as_secs_f32() <= window)
    }

    /// Drop only stamps older than `window`: the hidden→visible boundary
    /// retirement. A TUI completes a hidden boundary mid-burst (DECTCEM hide
    /// inside a DEC-2026 bracket, per keystroke), so stamps still within their
    /// freshness window are real keys whose echoes are in flight and survive.
    /// Packing is preserved: the stale prefix is removed and the fresh suffix
    /// shifts down.
    pub(crate) fn retire_stale(&mut self, now: Instant, window: f32) {
        self.retain(|t| now.saturating_duration_since(t).as_secs_f32() <= window);
    }

    /// The oldest still-fresh stamp, left in place: what [`Self::take_fresh`]
    /// would consume. The classifier peeks, then consumes once the echo's shape
    /// shows the stamp's own press is in it.
    pub(crate) fn peek_fresh(&self, now: Instant, window: f32) -> Option<Instant> {
        self.slots
            .iter()
            .map_while(|s| *s)
            .find(|t| now.saturating_duration_since(*t).as_secs_f32() <= window)
    }

    /// Consume the oldest still-fresh stamp (one stamp, one echo sweep —
    /// keys license echoes in press order). Stale stamps are left in place.
    pub(crate) fn take_fresh(&mut self, now: Instant, window: f32) -> Option<Instant> {
        let idx = self
            .slots
            .iter()
            .map_while(|s| *s)
            .position(|t| now.saturating_duration_since(t).as_secs_f32() <= window)?;
        let taken = self.slots[idx];
        self.slots.copy_within(idx + 1.., idx);
        self.slots[TYPED_STAMP_DEPTH - 1] = None;
        taken
    }
}

/// One live unpaid press as the exact-run proofs collect it: `(slot, press
/// instant, unpaid cells, exact glyph if known)`.
pub(super) type LiveUnpaidPress = (usize, Instant, u8, Option<char>);

/// Bound on the exact-glyph proof for a loaded host that missed a whole
/// composer row: the same 128 credits an ordinary typed sweep can spend. Older
/// credits may precede the run, but none may be skipped inside it.
pub(super) const EXACT_COALESCED_PREFIX_MAX: usize = TYPED_STAMP_DEPTH;
pub(super) const EXACT_COALESCED_STALE_PREFIX_MAX: usize = 8;
/// At most two queued keys can be proved after an unknown insert (one cell per
/// frame, or one exact two-cell row transition); longer runs are ambiguous.
pub(super) const ORPHAN_EXACT_KEYS_MAX: usize = 2;
/// Fixed, press-ordered escrow candidates recovered after an unknown insert.
pub(super) type OrphanExactPressRun = [Option<(Instant, char)>; ORPHAN_EXACT_KEYS_MAX];

/// The press-credit ring: the typed-echo coalescer's ledger, one slot per keyed
/// glyph (`(press instant, unpaid cells, exact glyph if known)`), banked by
/// [`CursorGlow::note_typed_glyph`] and spent oldest-first by the cells a
/// licensed typed echo lays. The question is whether a press's cells have been
/// laid, not how old it is: a late batch is always older than its presses'
/// observation.
///
/// A slot leaves the ring when its cells are laid, when an edge the press
/// cannot explain forgets it ([`Self::forget`],
/// [`CursorGlow::forget_typed_credits`]), when the key that erases it is
/// pressed ([`Self::retire_newest`]), or after [`IN_FLIGHT_PATIENCE_S`]; every
/// read is life-filtered ([`Self::within`]). A scroll or row-band move keeps
/// the pool. Fixed-size and `Copy`: the frame path allocates nothing.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PressCredits {
    pub(super) slots: [Option<(Instant, u8, Option<char>)>; TYPED_STAMP_DEPTH],
    /// The next slot to write; when full the ring overwrites its oldest press.
    /// Only [`Self::bank`] moves it and other mutators blank slots in place, so
    /// walking from `head` visits live presses oldest-first, the order
    /// [`Self::spend_counting`] spends in.
    pub(super) head: usize,
    /// The held presses: every unpaid press banked at or before this instant
    /// was kept by the hand's-row hold ([`CursorGlow::hold_the_live_presses`]),
    /// after a run below the caret ended on a glyph no live press typed. That
    /// run is either a footer tick inside a hesitated key's window or the keys'
    /// own echo in an echo area (Emacs `C-u 8-`), indistinguishable at the
    /// refusal. A held press stays redeemable for the in-flight patience, but
    /// only by a move on the hand's row ([`Self::admit_held`]). Forgotten with
    /// the rest by every forget edge.
    pub(super) held_through: Option<Instant>,
    /// Whether the move being judged stays on the hand's row, so held presses
    /// may pay for it; set only during that judgment
    /// ([`CursorGlow::judging_on_rows`]).
    pub(super) admit_held: bool,
}

impl Default for PressCredits {
    // Written out because `[T; N]: Default` stops at N = 32.
    fn default() -> Self {
        Self {
            slots: [None; TYPED_STAMP_DEPTH],
            head: 0,
            held_through: None,
            admit_held: false,
        }
    }
}

impl PressCredits {
    /// Bank one press worth `credits` cells.
    pub(super) fn bank(&mut self, now: Instant, credits: u8, glyph: Option<char>) {
        self.slots[self.head] = Some((now, credits, glyph));
        self.head = (self.head + 1) % TYPED_STAMP_DEPTH;
    }

    /// Hold every press banked through `at` ([`Self::held_through`]).
    pub(super) fn hold_through(&mut self, at: Instant) {
        self.held_through = self.held_through.max(Some(at));
    }

    /// Whether the press banked at `at` is held ([`Self::held_through`]).
    pub(super) fn held(&self, at: Instant) -> bool {
        self.held_through.is_some_and(|through| at <= through)
    }

    /// Whether the press banked at `at` with `credits` unpaid cells may pay
    /// for the move being judged: unpaid, younger than
    /// [`IN_FLIGHT_PATIENCE_S`], and free — or held while the judged move
    /// stays on the hand's row ([`Self::admit_held`]).
    pub(super) fn payable(&self, at: Instant, credits: u8, now: Instant) -> bool {
        credits > 0
            && now.saturating_duration_since(at).as_secs_f32() <= IN_FLIGHT_PATIENCE_S
            && (self.admit_held || !self.held(at))
    }

    /// The live presses: unpaid, younger than [`IN_FLIGHT_PATIENCE_S`], and
    /// payable for the move being judged ([`Self::payable`]).
    pub(super) fn within(
        &self,
        now: Instant,
    ) -> impl Iterator<Item = (Instant, u8, Option<char>)> + '_ {
        self.slots
            .iter()
            .flatten()
            .copied()
            .filter(move |(t, c, _)| self.payable(*t, *c, now))
    }

    /// Unpaid cells of the live presses whose instant `keep` admits: the press
    /// budget the anti-stray gates read, partitioned by the caller's key clock
    /// (before a stamp: presses whose glyphs lie left of that key's cell; at or
    /// after it: the key's own press and later; at or before a park: the
    /// presses in flight when it was observed).
    pub(super) fn cells_where(&self, now: Instant, keep: impl Fn(Instant) -> bool) -> usize {
        self.within(now)
            .filter(|(t, _, _)| keep(*t))
            .map(|(_, c, _)| usize::from(c))
            .sum()
    }

    /// Unpaid cells of every live press.
    pub(super) fn cells_within(&self, now: Instant) -> usize {
        self.cells_where(now, |_| true)
    }

    /// Unpaid presses — the same pool counted in keys.
    pub(super) fn presses_within(&self, now: Instant) -> usize {
        self.within(now).count()
    }

    /// Whether any unpaid press is live at `now`.
    pub(super) fn any_within(&self, now: Instant) -> bool {
        self.within(now).next().is_some()
    }

    /// The oldest unpaid press, or `None`: the typed licence of last resort for
    /// a same-row forward echo that landed after every stamp went stale
    /// (`classify_move`). Oldest, because presses license echoes in order.
    pub(super) fn oldest_unpaid(&self, now: Instant) -> Option<Instant> {
        // The free presses are paid before the held ones
        // ([`Self::spend_counting`]), so the oldest of them is the next.
        let (mut free, mut held) = (None::<Instant>, None::<Instant>);
        for (t, _, _) in self.within(now) {
            let slot = if self.held(t) { &mut held } else { &mut free };
            *slot = Some(slot.map_or(t, |prev| prev.min(t)));
        }
        free.or(held)
    }

    /// The last press still funding a foreign-row park; once it ages out the
    /// park can retire before its own deadline.
    pub(super) fn newest_unpaid(&self, now: Instant) -> Option<Instant> {
        self.within(now).map(|(t, _, _)| t).max()
    }

    /// Whether an unpaid press stamped exactly `glyph` within `freshness_s`:
    /// the key's own echo, read off the glass. Any press in the window counts,
    /// since a coalesced echo leaves its last key's glyph beside the caret. A
    /// caller passing the full in-flight patience must also prove the old
    /// content moved with the glyph. A press with no stamped glyph witnesses
    /// nothing.
    pub(super) fn fresh_glyph(&self, now: Instant, freshness_s: f32, glyph: char) -> bool {
        self.within(now).any(|(at, _, g)| {
            g == Some(glyph) && now.saturating_duration_since(at).as_secs_f32() <= freshness_s
        })
    }

    /// The oldest fresh press's exact glyph, in ring order. Older unspent
    /// credits (surviving a TUI's suppressed echoes) are not witnesses for a
    /// new print, and a later typeahead key cannot replace the earlier glyph.
    pub(super) fn oldest_fresh_glyph(
        &self,
        now: Instant,
        freshness_s: f32,
    ) -> Option<(usize, char)> {
        for k in 0..TYPED_STAMP_DEPTH {
            let i = (self.head + k) % TYPED_STAMP_DEPTH;
            if let Some((at, credits, glyph)) = self.slots[i]
                && self.payable(at, credits, now)
                && now.saturating_duration_since(at).as_secs_f32() <= freshness_s
            {
                return glyph.map(|glyph| (i, glyph));
            }
        }
        None
    }

    /// Whether `printed` is foreign to every press in flight: `Some(true)` when
    /// at least one press is live, all carry exact glyphs
    /// ([`CursorGlow::note_typed_expected`]) and none is `printed`;
    /// `Some(false)` when one is. `None` for an empty pool or any glyph-less
    /// live press (wide glyph, IME commit, count-only host): nothing may be
    /// refused on it.
    pub(super) fn foreign_to_live(&self, now: Instant, printed: char) -> Option<bool> {
        let mut any = false;
        for (_, _, glyph) in self.within(now) {
            match glyph {
                None => return None,
                Some(g) if g == printed => return Some(false),
                Some(_) => any = true,
            }
        }
        any.then_some(true)
    }

    /// The chronological unpaid one-cell keys must spell this whole small
    /// observed run. A stalled compositor may show several echoes in one frame
    /// after the oldest key left the short hint window; the in-flight ledger
    /// still holds the exact sequence. Only credits already stale at the prior
    /// blank-row sample may be skipped before the run; none inside it.
    pub(super) fn exact_unpaid_run(
        &self,
        now: Instant,
        previous_probe_at: Instant,
        glyphs: &[char],
    ) -> Option<usize> {
        self.exact_unpaid_run_with_tail_window(
            now,
            previous_probe_at,
            glyphs,
            CursorGlow::TYPE_HINT_FRESH,
        )
    }

    /// The same proof with a caller-selected tail age. A same-caret
    /// blank-to-glyph transition witnesses a single delayed echo; a moving
    /// coalesced prefix needs its final key fresh. Earlier credits may be
    /// skipped only when they predate the prior row probe and the short hint.
    pub(super) fn exact_unpaid_run_with_tail_window(
        &self,
        now: Instant,
        previous_probe_at: Instant,
        glyphs: &[char],
        tail_window_s: f32,
    ) -> Option<usize> {
        if glyphs.is_empty() || glyphs.len() > EXACT_COALESCED_PREFIX_MAX {
            return None;
        }
        // A fixed stack ledger holds the whole candidate plus only the bounded
        // older-credit prefix. More pending keys are ambiguous and fail closed.
        let mut live: [Option<LiveUnpaidPress>;
            EXACT_COALESCED_PREFIX_MAX + EXACT_COALESCED_STALE_PREFIX_MAX] =
            [None; EXACT_COALESCED_PREFIX_MAX + EXACT_COALESCED_STALE_PREFIX_MAX];
        let mut n = 0;
        for k in 0..TYPED_STAMP_DEPTH {
            let i = (self.head + k) % TYPED_STAMP_DEPTH;
            let Some((at, credits, glyph)) = self.slots[i] else {
                continue;
            };
            if !self.payable(at, credits, now) {
                continue;
            }
            if n == live.len() {
                return None;
            }
            live[n] = Some((i, at, credits, glyph));
            n += 1;
        }
        for start in 0..n {
            if start + glyphs.len() <= n {
                let exact = glyphs.iter().enumerate().all(|(offset, &expected)| {
                    live[start + offset].is_some_and(|(_, _, credits, glyph)| {
                        credits == 1 && glyph == Some(expected)
                    })
                });
                if exact {
                    let (_, last_at, _, _) = live[start + glyphs.len() - 1]?;
                    // An old exact phrase cannot borrow an unrelated fresh
                    // key's hint to claim a later program redraw.
                    if now.saturating_duration_since(last_at).as_secs_f32() <= tail_window_s {
                        return live[start].map(|(slot, _, _, _)| slot);
                    }
                }
            }
            // A failed candidate may be passed only if its first credit is
            // older than the sampled blank row and the short hint window; a
            // valid delayed echo above wins first.
            let (_, at, _, _) = live[start]?;
            if start >= EXACT_COALESCED_STALE_PREFIX_MAX
                || at > previous_probe_at
                || now.saturating_duration_since(at).as_secs_f32() <= CursorGlow::TYPE_HINT_FRESH
            {
                return None;
            }
        }
        None
    }

    /// The slot of the oldest payable press when the payable presses begin,
    /// oldest-first, with exactly `glyphs`: one cell each, every one pressed
    /// after `after` ([`CursorGlow::merged_correction`]).
    pub(super) fn exact_run_after(
        &self,
        now: Instant,
        after: Instant,
        glyphs: &[char],
    ) -> Option<usize> {
        let mut first = None;
        let mut n = 0;
        for k in 0..TYPED_STAMP_DEPTH {
            if n == glyphs.len() {
                break;
            }
            let i = (self.head + k) % TYPED_STAMP_DEPTH;
            let Some((at, credits, glyph)) = self.slots[i] else {
                continue;
            };
            if !self.payable(at, credits, now) {
                continue;
            }
            if at <= after || credits != 1 || glyph != Some(glyphs[n]) {
                return None;
            }
            first.get_or_insert(i);
            n += 1;
        }
        first.filter(|_| n == glyphs.len() && n > 0)
    }

    /// A later exact content echo proves earlier same-stream keys already
    /// appeared or were suppressed: drop their unpaid credits so they cannot
    /// fund unrelated program output. The slot is identified by ring
    /// position, since two keys can share an input timestamp.
    pub(super) fn retire_before_slot(&mut self, selected: usize) {
        for k in 0..TYPED_STAMP_DEPTH {
            let i = (self.head + k) % TYPED_STAMP_DEPTH;
            if i == selected {
                return;
            }
            self.slots[i] = None;
        }
    }

    /// Spend `cells` oldest-first: the presses that produced this echo are
    /// consumed, so one pool of typing never funds a second echo. Returns how
    /// many presses drained to zero.
    ///
    /// Walks from [`Self::head`] (only `bank` moves it; other mutators blank
    /// slots), so presses are visited oldest-first without rescans. Free
    /// presses pay before held ones ([`Self::held_through`]); otherwise the
    /// newer key's credit would stay free to fund a hop anywhere.
    pub(super) fn spend_counting(&mut self, now: Instant, mut cells: usize) -> usize {
        let mut drained = 0;
        for held in [false, true] {
            for k in 0..TYPED_STAMP_DEPTH {
                if cells == 0 {
                    return drained;
                }
                let i = (self.head + k) % TYPED_STAMP_DEPTH;
                let Some((t, c, _)) = self.slots[i] else {
                    continue;
                };
                if self.held(t) != held || !self.payable(t, c, now) {
                    continue;
                }
                let take = usize::from(c).min(cells);
                cells -= take;
                if usize::from(c) == take {
                    self.slots[i] = None;
                    drained += 1;
                } else if let Some((_, left, _)) = &mut self.slots[i] {
                    *left -= take as u8;
                }
            }
        }
        drained
    }

    /// Blank every slot whose press instant `pred` admits, in place (the
    /// head does not move, so the chronological walk still holds).
    pub(super) fn blank_where(&mut self, pred: impl Fn(Instant) -> bool) {
        for slot in self.slots.iter_mut() {
            if slot.is_some_and(|(t, _, _)| pred(t)) {
                *slot = None;
            }
        }
    }

    /// Drop every press past [`IN_FLIGHT_PATIENCE_S`] — the clock's bound,
    /// applied physically at a hidden→visible boundary.
    pub(super) fn retire_stale(&mut self, now: Instant) {
        self.blank_where(|t| now.saturating_duration_since(t).as_secs_f32() > IN_FLIGHT_PATIENCE_S);
    }

    /// Revoke the press banked at `at`, for a dispatch that never reached the
    /// child (a failed inline write, a key queued behind a draining paste). A
    /// press the tty never saw is neither a licence nor a credit.
    pub(super) fn revoke_at(&mut self, at: Instant) {
        self.blank_where(|t| t == at);
    }

    /// Forget every press — an observed edge the presses cannot explain.
    pub(super) fn forget(&mut self) {
        self.slots = [None; TYPED_STAMP_DEPTH];
        self.head = 0;
        self.held_through = None;
    }

    /// Retire every press banked at or before `at`: the twin of
    /// [`Self::revoke_at`] for presses an unknown-width insert's hop echoed
    /// without pricing ([`InsertSeam::orphans`]). Such presses can only echo in
    /// that hop or the next repaint, so they are retired rather than left to
    /// license a keyless `+1`. Later presses are untouched.
    pub(super) fn retire_through(&mut self, at: Instant) {
        self.blank_where(|t| t <= at);
    }

    /// One or two exact one-cell keys dispatched after an insert but banked by
    /// its delivery, in press order. Their exact blank-to-glyph echoes may
    /// prove them singly or as one two-cell transition; a wider or non-exact
    /// run stays ambiguous.
    pub(super) fn exact_run_between(
        &self,
        now: Instant,
        dispatched_at: Instant,
        delivered_at: Instant,
    ) -> Option<(OrphanExactPressRun, usize)> {
        let mut keys = [None; ORPHAN_EXACT_KEYS_MAX];
        let mut n = 0;
        for k in 0..TYPED_STAMP_DEPTH {
            let i = (self.head + k) % TYPED_STAMP_DEPTH;
            let Some((at, credits, glyph)) = self.slots[i] else {
                continue;
            };
            if at <= dispatched_at || at > delivered_at || !self.payable(at, credits, now) {
                continue;
            }
            if n == keys.len() || credits != 1 {
                return None;
            }
            let glyph = glyph?;
            // A blank cell cannot witness a space or NUL echo: accepting it
            // would let an unrelated program move spend this queued key.
            if matches!(glyph, ' ' | '\0') {
                return None;
            }
            keys[n] = Some((at, glyph));
            n += 1;
        }
        (n > 0).then_some((keys, n))
    }

    /// Take every press banked after `at` out of the ring at its own index:
    /// the twin of [`TypedStamps::split_off_after`] for a held park's flush.
    /// The head is copied so [`Self::merge`] can walk them oldest-first.
    pub(super) fn split_off_after(&mut self, at: Instant) -> Self {
        let mut later = Self {
            head: self.head,
            ..Self::default()
        };
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_some_and(|(t, _, _)| t > at) {
                later.slots[i] = slot.take();
            }
        }
        later
    }

    /// Put back what [`Self::split_off_after`] took. The judgment never banks,
    /// so presses return to their own slots; if it forgot the ring (head reset,
    /// all blank) they are re-banked oldest-first from the copied head.
    pub(super) fn merge(&mut self, later: Self) {
        if self.head == 0 && self.slots.iter().all(Option::is_none) {
            for k in 0..TYPED_STAMP_DEPTH {
                let i = (later.head + k) % TYPED_STAMP_DEPTH;
                if let Some((t, c, glyph)) = later.slots[i] {
                    self.bank(t, c, glyph);
                }
            }
            return;
        }
        for (i, slot) in later.slots.into_iter().enumerate() {
            if slot.is_some() {
                self.slots[i] = slot;
            }
        }
    }

    /// The slot of the newest press, if any.
    pub(super) fn newest(&self) -> Option<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.map(|(t, _, _)| (i, t)))
            .max_by_key(|&(_, t)| t)
            .map(|(i, _)| i)
    }

    /// Retire the newest press (the key a Backspace erases), whole, however
    /// many cells it was priced at.
    pub(super) fn retire_newest(&mut self) {
        if let Some(i) = self.newest() {
            self.slots[i] = None;
        }
    }

    /// Retire `cells` of the newest press: one glyph out of a press that banked
    /// several. A committed IME run is one press priced at its summed width
    /// ([`CursorGlow::note_typed_cells`]); a Backspace erases one glyph, so the
    /// slot goes only when its last cell is paid. An erase wider than the slot
    /// takes the slot and no more. A zero-cell erase (a zero-width cluster)
    /// takes nothing, leaving the press before the run standing.
    pub(super) fn retire_newest_cells(&mut self, cells: u8) {
        if cells == 0 {
            return;
        }
        if let Some(i) = self.newest()
            && let Some((_, c, _)) = &mut self.slots[i]
        {
            *c = c.saturating_sub(cells);
            if *c == 0 {
                self.slots[i] = None;
            }
        }
    }

    /// Whether no press is banked at all (spent or unspent — a slot with
    /// zero credits left is `None`).
    pub(super) fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }
}

/// A queued key's licence class, re-stamped at delivery
/// ([`CursorGlow::note_delivered`]): what the key banked at dispatch and the
/// enqueue revoked. A delivered insert is not a key class; its witness is the
/// completed write ([`InsertWidth`], [`CursorGlow::note_insert_delivered_from`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveredClass {
    /// A plain typed glyph: its stamp (the credit and v2 press were never
    /// revoked).
    Typed,
    /// A plain Enter: the Return one-shot.
    Return,
    /// A composer newline (Shift+Enter on the alternate screen).
    Newline,
    /// A navigation key: the nav one-shot.
    Nav,
    /// A Backspace: the quench and poof stamps and the pre-erase row fill.
    Erase,
    /// A kill chord: its poof witness, whether its echo relocates the caret
    /// (then the nav stamp that licenses the retreat and the one retreat it
    /// is owed, [`CursorGlow::kill_retreat_pending`]) and whether it is
    /// word-scale.
    Kill { moves_cursor: bool, word: bool },
    /// A Tab (any modifier) or a bare ⌃V: the gesture class, beside the
    /// delivered insert the same receipt carries (cross-row completion is the
    /// gesture's, the same-row sweep the insert's).
    Gesture,
}

/// How the host priced a delivered insert ([`CursorGlow::note_insert_delivered_from`]).
/// The two classes are spent under different row laws, so the distinction is a
/// type rather than a sentinel width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertWidth {
    /// Priced in cells from the very text put on the wire
    /// (`Terminal::paste_insert_cells`). `Cells(0)` arms nothing.
    Cells(u16),
    /// One line priced in cells, then the one line break that ends it (the
    /// GUI's `paste_insert_width`: a link copied with its line ending).
    /// Its echo is the line's cells on the row the hand was on when armed and
    /// the break's new line on the row under it, often in one frame
    /// ([`CursorGlow::line_insert_echo`]); a host that swallows the break
    /// echoes it as [`Self::Cells`]. `Line(0)` is the unknown class.
    Line(u16),
    /// The text cannot say: a Tab's completion, a raw ⌃V, a multi-line or
    /// tabbed body, a body past the price probe. Bounded at
    /// [`CursorGlow::INSERT_GESTURE_CELLS`] and spendable only on the row the
    /// hand was on when armed.
    Unknown,
}

/// The delivered-insert licence: a human insert gesture (file drop, ⌘V, the
/// `paste` control verb, an in-app ⌃V image insert, a Tab completion) whose
/// bytes provably landed on the wire, priced in cells by the host. Its witness
/// is delivery, not a keypress: in the PTY stream a paste's echo and program
/// output are the same bytes. One-shot, fresh for
/// [`CursorGlow::INSERT_HINT_FRESH`], and spendable only by a same-row forward
/// advance no wider than `cells` plus the unpaid typed credits behind it, on
/// the insert's own row ([`CursorGlow::insert_row_identity`]). See
/// [`CursorGlow::note_insert_delivered_from`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InsertLicence {
    /// The delivery instant — the writer thread's completed write (or the
    /// dispatch instant for a synchronous insert such as Tab).
    pub(super) at: Instant,
    /// When the insert's first byte was dispatched: keys pressed after this
    /// cannot own an echo of its bytes, even if already banked.
    pub(super) dispatched_at: Instant,
    /// The insert's cell width as the host priced it, or the
    /// [`CursorGlow::INSERT_GESTURE_CELLS`] bound when it could not. Accumulates
    /// across inserts delivered before the first was spent (one `Paste` per
    /// dropped file).
    pub(super) cells: u16,
    /// Whether `cells` is a priced width ([`InsertWidth::Cells`]); an
    /// accumulation is priced only when every part was.
    pub(super) known: bool,
    /// Whether the priced line ends in its line break ([`InsertWidth::Line`]).
    /// An accumulation holding a break is a multi-line body: the unknown
    /// class.
    pub(super) breaks: bool,
    /// The row witness: the row the hand was on when the licence was armed
    /// ([`CursorGlow::insert_row`]: the visible caret's row, unless the
    /// anchored lane established the echo row elsewhere, else the last
    /// licensed move's row within the chain window), `None` when nothing could
    /// say. It is the only identity an unknown-width insert has, and keeps a
    /// Tab's stamp off a prompt printed on another row.
    pub(super) row: Option<u16>,
}

/// Where a delivered insert's sweep was laid: the rewrite window's memory. A
/// TUI that swaps inserted text for a placeholder (Claude Code turning a
/// dropped image path into `[Image #1] `) pulls the caret back inside this span
/// with no key; the seam reads that as the insert's rewrite and retracts the
/// ribbon to the new caret ([`rk::Event::Rewrite`]). Row-addressed like
/// [`AnchorRow`]: dropped on scroll, row-band move and reset; stale after
/// [`CursorGlow::INSERT_REWRITE_FRESH`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InsertSpan {
    pub(super) row: u16,
    pub(super) col0: u16,
    pub(super) col1: u16,
    pub(super) laid_at: Instant,
}

/// A same-row forward hop the seam refused for want of a licence, remembered
/// for one delivery. A preempted writer thread can publish an insert's receipt
/// after the frame that observed its echo; without this the whole insert would
/// stay dark. When [`CursorGlow::note_insert_delivered_from`] finds a refused hop of
/// exactly the insert's (accumulated) width within
/// [`CursorGlow::INSERT_RETRO_FRESH`], it lays it. An unknown-width insert
/// claims the hop by its row witness instead, up to
/// [`CursorGlow::INSERT_GESTURE_CELLS`]. Any other hop is forgotten.
/// Row-addressed: dropped on scroll, row-band move and reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PendingHop {
    pub(super) row: u16,
    pub(super) col0: u16,
    pub(super) col1: u16,
    pub(super) at: Instant,
}

/// A visible-lane typed lay whose caret has not rested. Earlier observed
/// parked rows are refused before laying ([`CursorGlow::unowned_caret`]);
/// this is the fallback when that earlier evidence was unavailable. A later
/// print off the row followed by a keyless rest on or left of the new cells
/// recalls them through [`rk::Event::Rewrite`]. Judged at the caret's rest
/// ([`CursorGlow::TORN_REST_S`] without a move) or the next typed lay's origin.
/// A keyed move, an established typing row, a rest right of the lay, or a
/// redraw confined to the lay's row keeps it.
#[derive(Clone, Copy, Debug)]
pub(super) struct TornLay {
    /// The row the typed echo laid on (a scroll drops the lay).
    pub(super) row: u16,
    /// The lay's first cell.
    pub(super) col0: u16,
    /// The caret the echo left the row at (exclusive end of the lay).
    pub(super) col1: u16,
    /// The caret last seen while armed (`None`: hidden).
    pub(super) seen: Option<(u16, u16)>,
    /// When the caret last moved (the lay itself included).
    pub(super) moved_at: Instant,
    /// The hand's row before the lay put the hand on `row`.
    pub(super) hand_before: Option<HandRow>,
    /// The generic boundary's doubt before the lay settled it
    /// ([`CursorGlow::hand_lands_on`]); a recall restores it with the hand.
    pub(super) unowned_before: Option<UnownedCaret>,
    /// The print anchor's `seq` at the lay.
    pub(super) anchor_seq: Option<u64>,
    /// A print since the lay ended on another row: the program wrote
    /// elsewhere. Without it nothing is recalled.
    pub(super) printed_off_row: bool,
}

/// The most presses an [`EraseWitness`] remembers retiring.
pub(super) const ERASE_RETIRED_MAX: usize = 4;

/// The last Backspace ([`CursorGlow::note_backspace_erasing`]) and what its
/// echo still owes. Bytes are processed in order: the keys typed before the
/// Backspace echo ahead of its retreat, a key it retired before that key
/// echoed included, and no key typed after it echoes before it. So until a
/// caret move shows the erase, the erase licenses its own retreat however
/// late, and a forward hop printing a retired key's glyph is that key's late
/// echo, which no press pays ([`CursorGlow::erase_hop`]). In that time the
/// erase and the keys typed since can also echo as one repaint
/// ([`CursorGlow::merged_correction`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct EraseWitness {
    /// The latest Backspace's press.
    pub(super) at: Instant,
    /// The glyphs of the presses Backspaces retired before they echoed, in
    /// typing order, their late echoes still to land.
    pub(super) retired: [char; ERASE_RETIRED_MAX],
    pub(super) retired_n: u8,
    /// The retreat cells the Backspaces since the last shown erase still owe
    /// (an unpriced erase owes one).
    pub(super) owed: u16,
}

impl EraseWitness {
    /// Whether a retreat is still owed at `now`, within the press patience.
    pub(super) fn owed_at(&self, now: Instant) -> bool {
        self.owed > 0
            && now.saturating_duration_since(self.at).as_secs_f32() <= IN_FLIGHT_PATIENCE_S
    }

    /// The retired glyphs still to echo.
    pub(super) fn retired(&self) -> &[char] {
        &self.retired[..usize::from(self.retired_n)]
    }
}

/// What an observed move is to an owed [`EraseWitness`]
/// ([`CursorGlow::erase_hop`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EraseHop {
    /// A same-row retreat no wider than the erases owe: their echo, which
    /// they license.
    Retreat(u16),
    /// A same-row forward hop the presses typed before the erase pay: theirs,
    /// echoed ahead of it. The erase stays owed.
    Ahead,
    /// A same-row forward hop whose last `n` cells, past what those presses
    /// pay, print the retired keys' glyphs: their late echo, which no press
    /// pays and the erase licenses. The erase stays owed.
    Late(u16),
    /// Any other move: the erase is past.
    Past,
}

/// The park: Ink repaints the input row by parking the caret left (a same-row
/// backward move), then rewriting the row to its end plus the new glyph, and a
/// frame can catch the two ~40 ms apart. Judged at once, the park would be a
/// typed re-anchor and the return refused for want of a licence: half the row
/// dark. So a same-row backward typed-paired (or in-flight- or insert-backed)
/// move is held, not judged: nothing consumed or spent, no v2 `Move`.
///
/// Its return on either lane releases it ([`CursorGlow::release_held_park`]: a
/// forward move within one stamp window from the landing past the origin,
/// reclassified as `origin -> target`, or the rewrite's print advancing the
/// row's end from the origin under a hidden caret); a same-end rewrite cancels
/// it; every other edge flushes it at its own clock through the ordinary body,
/// so every other verdict is unchanged, only up to one observed move (or
/// 0.25 s) later. It rides a scroll or band move with its row
/// ([`CursorGlow::carry_held_park`]) and is a wake source (`is_active`,
/// `next_change_deadline`), so a host sleeping on the engine's word still
/// delivers a silent park's verdict. Rainbow Kitty only; `Copy`, carrying the
/// config and geometry it must be judged under.
#[derive(Clone, Copy)]
pub(super) struct HeldPark {
    /// The source row. A background footer repaint may park on another
    /// row while this row's typed prefix remains unchanged.
    pub(super) row: u16,
    pub(super) landing_row: u16,
    /// Content proved that a foreign-row landing left the source intact.
    /// Such a park can expire only dark, never on a saved-time licence.
    pub(super) cross_row: bool,
    pub(super) origin: u16,
    pub(super) landing: u16,
    pub(super) at: Instant,
    pub(super) cfg: GlowConfig,
    pub(super) geom: Geom,
    /// Whether a glyph sat at the landing cell (`landing - 1`) when the park
    /// was held, from the host's row probe. A flushed park is judged as a
    /// typed re-anchor whose landing sweep is for a box-growth wrap; for an Ink
    /// park to the input's start that cell is the prompt and must not light.
    /// Unknown (no probe, or another row's) reads as a glyph.
    pub(super) landing_glyph: bool,
    /// Which glyph sat at the landing cell when the park was held
    /// ([`CursorGlow::probe_glyph_before`]), `None` if unknown. The flush has
    /// no probe of its own frame, so this is the landing gate's witness there
    /// ([`MoveCtx::landing_foreign`]); e.g. zsh's Ctrl-R walking a match back
    /// along its row is held like Ink's park, and its landing cell was typed
    /// by no key.
    pub(super) landing_cell: Option<char>,
    /// The content witness carried this exact park's source row away.
    pub(super) content_followed: bool,
}

impl HeldPark {
    pub(super) fn patience(&self) -> Duration {
        if self.cross_row {
            Duration::from_secs_f32(IN_FLIGHT_PATIENCE_S)
        } else {
            Duration::from_secs_f32(CursorGlow::TYPE_HINT_FRESH)
        }
    }

    pub(super) fn fresh(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.at) <= self.patience()
    }
}

/// `trail status`'s delivered-insert rows: how many inserts the host
/// delivered, how many the seam lit, how many placeholder rewrites it
/// retracted, and the last insert's priced width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InsertTally {
    /// Inserts the host reported delivered, less any arm revoked at its exact
    /// instant before delivery (a Tab queued behind a draining paste counts
    /// once, at its re-arm).
    pub delivered: u64,
    /// Inserts whose echo the seam licensed and laid as one sweep.
    pub lit: u64,
    /// Program rewrites of a lit span the seam retracted.
    pub retracted: u64,
    /// The last delivered insert's cell width, of the last arm that stood (a
    /// revoked arm restores the width before it).
    pub last_cells: u16,
}

/// The delivered-insert seam: a delivery-witnessed licence with a row identity
/// ([`InsertLicence`]), the undo a revoked arm restores ([`InsertArmUndo`]),
/// the span the licence was laid as ([`InsertSpan`]), the refused hop one
/// delivery may retro-license ([`PendingHop`]), the presses an unknown width's
/// hop echoed, and `trail status`'s tally.
///
/// Invariants: the undo is written only at an arm; the span and hop are
/// row-addressed and dropped with the row space; the orphans clear with the
/// press pool. The licence survives class-changing supersedes (`clear_typed`,
/// a modifier release, a nav press), since its witness is the delivery and a
/// debounced TUI may echo it over a second later; it yields to a fresher class
/// at the seam ([`CursorGlow::fresher_class_owns_hop`]) and is revoked only by
/// exact instant, coordinate-space teardown, or expiry.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct InsertSeam {
    /// The licence armed by the host when an insert's bytes provably landed,
    /// spent by the one same-row forward echo that fits it.
    pub(super) armed: Option<InsertLicence>,
    /// What the newest arm overwrote, for the revoke of that arm only.
    pub(super) undo: InsertArmUndo,
    /// Where the last delivered insert was laid — the placeholder-rewrite
    /// window.
    pub(super) span: Option<InsertSpan>,
    /// The last refused same-row forward hop — one delivery may retro-license
    /// it.
    pub(super) pending_hop: Option<PendingHop>,
    /// The presses an unknown-width insert's hop echoed: `(delivery instant,
    /// tick that laid the hop)`. An unknown insert is priced at its
    /// [`CursorGlow::INSERT_GESTURE_CELLS`] bound, so typed presses queued
    /// behind it (`[Pasted text #1 +3 lines] k` echoing as one hop) would keep
    /// their credits and license a later keyless `+1`. Presses banked at or
    /// before the delivery can only echo in that hop or the next repaint, so
    /// they are retired ([`PressCredits::retire_through`]) once the hop is
    /// [`CursorGlow::INSERT_REWRITE_FRESH`] old ([`Self::retire_orphans`]). A
    /// key echoing the next frame spends its own credit first. Cleared with
    /// the pool.
    pub(super) orphans: Option<UnknownInsertOrphans>,
    /// Up to two ambiguous post-insert keys, removed from the press pool at
    /// orphan cleanup. Only their ordered exact glyphs at consecutive cells can
    /// claim them, singly or as one two-cell frame; any other movement
    /// discards the escrow.
    pub(super) orphan_exact: Option<OrphanExactRun>,
    /// `trail status`'s insert rows ([`InsertTally`]).
    pub(super) tally: InsertTally,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct UnknownInsertOrphans {
    pub(super) dispatched_at: Instant,
    pub(super) delivered_at: Instant,
    pub(super) laid_at: Instant,
    /// Absent when the insert lacked a sampled row/print generation or its
    /// coordinate space was rewritten or translated before cleanup.
    pub(super) site: Option<(u16, u16, u64)>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OrphanExactKey {
    pub(super) pressed_at: Instant,
    pub(super) row: u16,
    pub(super) col: u16,
    pub(super) glyph: char,
    pub(super) print_seq: u64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OrphanExactRun {
    pub(super) keys: [Option<OrphanExactKey>; ORPHAN_EXACT_KEYS_MAX],
    pub(super) next: usize,
    pub(super) len: usize,
    /// The inserted span's landing, fixed even after one key is proved.
    pub(super) insert_col: u16,
}

impl OrphanExactRun {
    pub(super) fn current(self) -> Option<OrphanExactKey> {
        self.keys.get(self.next).copied().flatten()
    }
}

/// What an arm overwrote, restored when that arm is revoked at its exact
/// instant (a Tab or ⌃V queued behind a draining paste). The revoke must not
/// take the paste's credit with it; a revoked arm was not a delivery.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct InsertArmUndo {
    /// The still-fresh, unspent licence the newest arm accumulated into.
    /// Dropped with the armed licence everywhere else.
    pub(super) licence: Option<InsertLicence>,
    /// `last_insert_cells=` before the newest arm; read only by that arm's
    /// revoke.
    pub(super) last_cells: u16,
}

impl InsertSeam {
    /// Arm a delivered insert of `cells` (`known` when priced, else the
    /// unknown bound) at `at`, on `row` ([`CursorGlow::insert_row`]). A second
    /// delivery before the first was spent accumulates: a multi-file drop
    /// arrives as one `Paste` per file and the shell echoes them as one hop.
    /// The sum is priced only if both parts were; the newer row witness wins
    /// when present; a stale stamp is replaced. The prior licence is kept as
    /// the undo for this arm's revoke.
    pub(super) fn arm(
        &mut self,
        at: Instant,
        dispatched_at: Instant,
        cells: u16,
        known: bool,
        breaks: bool,
        row: Option<u16>,
    ) -> InsertLicence {
        self.tally.delivered += 1;
        self.undo.last_cells = self.tally.last_cells;
        self.tally.last_cells = cells;
        let prior = self.armed.filter(|prev| {
            at.saturating_duration_since(prev.at).as_secs_f32() <= CursorGlow::INSERT_HINT_FRESH
        });
        let armed = match prior {
            // A line break in either part puts the parts on two lines: a
            // multi-line body, whose width the text cannot say.
            Some(prev) if prev.breaks || breaks => InsertLicence {
                at,
                dispatched_at: prev.dispatched_at.min(dispatched_at),
                cells: CursorGlow::INSERT_GESTURE_CELLS,
                known: false,
                breaks: false,
                row: row.or(prev.row),
            },
            Some(prev) => InsertLicence {
                at,
                dispatched_at: prev.dispatched_at.min(dispatched_at),
                // A priced part adds its exact width; the unknown bound counts
                // once per licence, so repeated unspent Tabs / ⌃V do not widen
                // it.
                cells: if known || prev.known {
                    prev.cells.saturating_add(cells)
                } else {
                    prev.cells
                },
                known: prev.known && known,
                breaks: false,
                row: row.or(prev.row),
            },
            None => InsertLicence {
                at,
                dispatched_at,
                cells,
                known,
                breaks,
                row,
            },
        };
        self.undo.licence = prior;
        self.armed = Some(armed);
        armed
    }

    /// Revoke the arm stamped at exactly `at` (a queued or failed dispatch):
    /// restore the licence it accumulated into, undo its `delivered` tick and
    /// `last_cells`. The re-arm at delivery counts the gesture once. Other
    /// instants are untouched.
    pub(super) fn revoke_arm_at(&mut self, at: Instant) {
        if self.armed.is_some_and(|i| i.at == at) {
            self.armed = self.undo.licence.take();
            self.tally.delivered = self.tally.delivered.saturating_sub(1);
            self.tally.last_cells = self.undo.last_cells;
        }
    }

    /// The armed licence while fresh: [`CursorGlow::INSERT_HINT_FRESH`] for a
    /// priced insert, the shorter [`CursorGlow::INSERT_GESTURE_HINT_FRESH`] for
    /// the unknown class.
    pub(super) fn fresh(&self, now: Instant) -> Option<InsertLicence> {
        self.armed.filter(|i| {
            let window = if i.known {
                CursorGlow::INSERT_HINT_FRESH
            } else {
                CursorGlow::INSERT_GESTURE_HINT_FRESH
            };
            now.saturating_duration_since(i.at).as_secs_f32() <= window
        })
    }

    /// Drop the licence and the credit it accumulated into.
    pub(super) fn clear(&mut self) {
        self.armed = None;
        self.undo.licence = None;
    }

    /// A coordinate-space teardown: the licence, its undo, the span and the
    /// hop go. The tally and undo width are readings and stay.
    pub(super) fn teardown(&mut self) {
        self.clear();
        self.span = None;
        self.pending_hop = None;
        self.orphans = None;
        self.orphan_exact = None;
    }

    /// Move the row-addressed members with a scroll or band move under `map`
    /// (absolute row to new row, `None` once off the grid or band). The span
    /// and pending hop are dropped like the anchor memory; the licence's row
    /// witness rides `map`, so a priced width is still recognised by width once
    /// its row is gone and an unknown one is not.
    pub(super) fn translate_rows(&mut self, map: impl Fn(u16) -> Option<u16>) {
        self.span = None;
        self.pending_hop = None;
        if let Some(orphans) = self.orphans.as_mut() {
            orphans.site = None;
        }
        self.orphan_exact = None;
        for ins in [&mut self.armed, &mut self.undo.licence]
            .into_iter()
            .flatten()
        {
            ins.row = ins.row.and_then(&map);
        }
    }

    /// Take the refused hop ([`PendingHop`]) if it is the insert `armed` now
    /// delivers: refused within [`CursorGlow::INSERT_RETRO_FRESH`] of `at`, at
    /// or after `dispatched_at` when known, and the insert's by width or row
    /// witness ([`Self::retro_hop_is_the_inserts`]). Matched against the
    /// accumulated licence, since two receipts straddling the echoing frame
    /// echo as one hop. The hop is forgotten either way.
    pub(super) fn take_retro_hop(
        &mut self,
        armed: InsertLicence,
        at: Instant,
        dispatched_at: Option<Instant>,
    ) -> Option<PendingHop> {
        let hop = self.pending_hop.take()?;
        (at.saturating_duration_since(hop.at).as_secs_f32() <= CursorGlow::INSERT_RETRO_FRESH
            && dispatched_at.is_none_or(|dispatched| hop.at >= dispatched)
            && Self::retro_hop_is_the_inserts(hop, armed.cells, armed.known, armed.row))
        .then_some(hop)
    }

    /// Whether a refused hop is the insert now delivered: a priced width by
    /// exactly its width; an unknown width by the row witness alone, bounded at
    /// [`CursorGlow::INSERT_GESTURE_CELLS`] (the terms
    /// [`CursorGlow::insert_row_identity`] spends the live echo on).
    pub(super) fn retro_hop_is_the_inserts(
        hop: PendingHop,
        cells: u16,
        known: bool,
        row: Option<u16>,
    ) -> bool {
        let width = hop.col1.saturating_sub(hop.col0);
        if known {
            width == cells
        } else {
            width > 0 && width <= CursorGlow::INSERT_GESTURE_CELLS && row == Some(hop.row)
        }
    }

    /// Whether a retreat to `col` on `row` lands inside the span a delivered
    /// insert laid within [`CursorGlow::INSERT_REWRITE_FRESH`]: the rewrite
    /// window's shape half (the rest is [`CursorGlow::insert_rewrite`]).
    pub(super) fn span_admits_retreat(&self, row: u16, col: u16, now: Instant) -> bool {
        self.span.is_some_and(|span| {
            span.row == row
                && col >= span.col0
                && col <= span.col1
                && now.saturating_duration_since(span.laid_at).as_secs_f32()
                    <= CursorGlow::INSERT_REWRITE_FRESH
        })
    }

    /// The licence was laid as `span`: take the one-shot and its credit, drop
    /// the refused hop, open the rewrite window, and count the lit insert.
    pub(super) fn laid(&mut self, span: InsertSpan) {
        self.clear();
        self.pending_hop = None;
        self.span = Some(span);
        self.orphan_exact = None;
        self.tally.lit += 1;
    }

    /// The laid span was retracted to the program's new caret `col`: the
    /// span memory shrinks to it, and `trail status` counts the rewrite.
    pub(super) fn retracted(&mut self, col: u16) {
        if let Some(span) = self.span.as_mut() {
            span.col1 = col;
        }
        if let Some(orphans) = self.orphans.as_mut() {
            orphans.site = None;
        }
        self.orphan_exact = None;
        self.tally.retracted += 1;
    }

    /// Once an unknown insert's hop is old enough for any next-frame echo to
    /// have spent its own credit ([`Self::orphans`]), return the delivery
    /// instant to retire the press ring through, once.
    pub(super) fn retire_orphans(&mut self, now: Instant) -> Option<UnknownInsertOrphans> {
        let orphans = self.orphans?;
        (now.saturating_duration_since(orphans.laid_at).as_secs_f32()
            > CursorGlow::INSERT_REWRITE_FRESH)
            .then(|| {
                self.orphans = None;
                orphans
            })
    }
}

/// `trail status`'s in-flight rows (the stall): `inflight_licensed=` /
/// `inflight_forgotten=` / `credits=` / `swallowed_no_echo=` /
/// `park_returns=` / `park_flushed=`. They show whether a stalled batch
/// reached the seam and how it was judged: licensed by the in-flight pool
/// alone, forgotten, withheld because the tty would never echo it, or a held
/// park returned or flushed. See [`CursorGlow::in_flight_tally`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InFlightTally {
    /// Moves licensed by the in-flight pool with no stamp fresh (the ring
    /// row reads `licence=inflight`).
    pub licensed: u64,
    /// Times an observed edge forgot a pool with a live press in it
    /// ([`CursorGlow::forget_typed_credits`]); teardown drops (focus loss,
    /// `reset`, the unseeded first draw, a declined hidden relocation) are not
    /// counted.
    pub forgotten: u64,
    /// The live pool: unpaid cells in flight at the last tick.
    pub credits: usize,
    /// Presses typed into a tty that will never echo them (canonical no-echo:
    /// `read -s`, `sudo`, an `ssh` passphrase, `passwd`), which bank nothing
    /// ([`CursorGlow::note_typed_swallowed_no_echo`]).
    pub swallowed_no_echo: u64,
    /// Held parks whose return came within the window and was judged as
    /// one `origin -> target` echo ([`HeldPark`]).
    pub park_returns: u64,
    /// Held parks flushed through the ordinary body at their own clock
    /// (no return: silence, an edge, or a move that was not the return).
    pub park_flushed: u64,
}

impl CursorGlow {
    /// Host key hint: one Backspace keypress. Escalates the quench meter, cools
    /// the standing heat and coal bed, and arms the quench classifier that
    /// licenses the following retreat as a deletion. Plain arrow-left never
    /// quenches.
    pub fn note_backspace(&mut self, now: Instant) {
        self.note_backspace_erasing(now, None);
    }

    /// [`Self::note_backspace`] with the erased glyph's cell width, priced by
    /// the host from the text it committed. A press is one key, but a
    /// committed IME run banks one press priced at its summed width
    /// ([`Self::note_typed_cells`]); a Backspace erases one glyph of it, so it
    /// pays back that glyph's cells and leaves the rest in flight.
    ///
    /// `Some(0)` is a priced zero (a zero-width cluster): nothing is paid back
    /// and every press stands. `None` (unpriceable erase) retires the whole
    /// newest slot. The two must stay distinct, or a zero-width erase would
    /// retire the press before the run.
    pub fn note_backspace_erasing(&mut self, now: Instant, cells: Option<u16>) {
        // Backspace starts a new deletion class: close every older movement
        // class and the swallowed typed cohort's unspent credits before arming
        // quench/poof state. `clear_typed_keeping_hand` flushes a held park
        // first, so its `Move` precedes the `Erase` in v2's order. The hand
        // keeps its row.
        self.clear_typed_keeping_hand(now);
        // The key it erases: a Backspace typed into a stall erases the newest
        // press in flight (bytes are processed in order), so that press is
        // retired and the rest keep waiting (`abc⌫d` echoes as three cells for
        // `a`, `b`, `d`). Not a forget: the row is still the hand's. An IME
        // press gives back only the erased glyph's cells when priced.
        let newest = self.type_press_ring.newest();
        let newest_press = newest.and_then(|i| self.type_press_ring.slots[i]);
        match cells {
            Some(cells) => self
                .type_press_ring
                .retire_newest_cells(u8::try_from(cells).unwrap_or(u8::MAX)),
            None => self.type_press_ring.retire_newest(),
        }
        // A press retired whole had not echoed yet, and its echo may still
        // land ahead of this erase's retreat ([`EraseWitness`]). An earlier
        // Backspace's retired presses stay owed while its retreat is unseen;
        // this one retired the newest press left, typed before theirs.
        let retired = newest
            .filter(|&i| self.type_press_ring.slots[i].is_none())
            .and(newest_press)
            .and_then(|(_, _, glyph)| glyph);
        let mut owed = self
            .last_erase
            .filter(|erase| erase.owed_at(now))
            .map_or(([' '; ERASE_RETIRED_MAX], 0, 0), |erase| {
                (erase.retired, erase.retired_n, erase.owed)
            });
        if let Some(glyph) = retired
            && usize::from(owed.1) < ERASE_RETIRED_MAX
        {
            owed.0.copy_within(..usize::from(owed.1), 1);
            owed.0[0] = glyph;
            owed.1 += 1;
        }
        // The two poof licences are one pending authority: a newer Backspace
        // must not borrow an unanswered kill's clause-scale fallback or swoosh.
        self.kill_hint = None;
        self.unsettle();
        self.quench = (self.quench + Self::QUENCH_GAIN).min(1.0);
        self.heat *= Self::QUENCH_COOL;
        self.coal *= Self::QUENCH_COOL;
        // Deletes never build momentum; they mildly drain it and spend the
        // pending gap credit ([`crate::typing_momentum`]). Stamped at the key,
        // so an echo the shell swallows still counts.
        self.momentum.delete(now);
        // The mirror metric builds on the same law from the same edge: one
        // erase is one advance of [`Self::erase_mom`].
        self.erase_mom.advance(now);
        self.quench_hint = Some(now);
        self.last_erase = Some(EraseWitness {
            at: now,
            retired: owed.0,
            retired_n: owed.1,
            owed: owed.2.saturating_add(cells.unwrap_or(1)),
        });
        // Schedule the plain-Backspace poof: `poof_scan` ORs this into the
        // `fresh_kill` gate, and the full-trust span/fallback machinery covers
        // the erase (a ContentOnly probe licenses nothing, see [`ProbeTrust`]).
        self.bs_poof_hint = Some(now);
        // Stamp the pre-erase row fill while it is still the pre-erase row
        // ([`Self::bs_baseline`]).
        self.bs_baseline = self.row_fill_at_key();
        // Seam point 2 (§17.2): one Backspace → v2's `Erase`, on the key's
        // own edge, beside the v1 bookkeeping above.
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::Erase, now);
        }
    }

    /// Host key hint: a navigation keypress (Ctrl-A/Ctrl-E, Home/End, arrows,
    /// word/line motions). It licenses the following move and classifies it
    /// as scrubbing: no heat, no meteor. Unlike [`Self::note_backspace`] it
    /// never touches heat or coal.
    pub fn note_navigation(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.unowned_caret = None;
        self.flush_held_park();
        if self.newline_hint.take().is_some() && self.v2.engaged() {
            self.v2.on_event(rk::Event::CancelComposerNewline, now);
        }
        self.unsettle();
        // A stronger input class supersedes a swallowed Tab/paste, so the
        // weaker hint cannot license an unrelated program move later.
        self.user_gesture_hint = None;
        self.nav_hint = Some(now);
    }

    /// Host key hint: one plain typed-glyph keypress (Character/Space echoes
    /// only; never Enter, Tab, navigation or modified chords). It licenses the
    /// following move and classifies a paired one-row move whose delta exceeds
    /// the typed advance as a typed re-anchor: a TUI input box (Claude Code's
    /// Ink prompt) rewraps per keystroke, so the caret lands left of its
    /// launch column without travelling (see `classify_move`).
    pub fn note_typed(&mut self, now: Instant) {
        self.note_typed_cells(now, 1);
    }

    /// [`Self::note_typed`] with the committed text's cell width: wide glyphs
    /// at 2, an IME commit at its summed cells, so CJK typing is not refused
    /// for budget (see [`Self::type_press_ring`]). Clamped to the sweep max.
    /// Plain keyboard input goes through the 1-cell [`Self::note_typed`].
    pub fn note_typed_cells(&mut self, now: Instant, cells: u16) {
        self.note_typed_glyph(now, cells, false, rk::TypedClass::Glyph);
    }

    /// [`Self::note_typed_cells`] with the glyph spelled by the host, which
    /// Rainbow Kitty v2's hero deal needs (§5.8: a shifted capital or `!` earns
    /// an m1; a space lays ribbon and deals no star) and an echo cannot tell.
    /// Only seam point 2 reads the extra fields; the wrapper is otherwise
    /// identical.
    pub fn note_typed_glyph(
        &mut self,
        now: Instant,
        cells: u16,
        shifted: bool,
        class: rk::TypedClass,
    ) {
        self.note_typed_glyph_with_expected(now, cells, shifted, class, None);
    }

    pub(super) fn note_typed_glyph_with_expected(
        &mut self,
        now: Instant,
        cells: u16,
        shifted: bool,
        class: rk::TypedClass,
        expected: Option<char>,
    ) {
        self.unsettle();
        self.supersede_typed_press();
        self.type_hint.stamp(now);
        let credits = cells.clamp(1, Self::RAINBOW_TYPED_SWEEP_MAX as u16) as u8;
        self.type_press_ring.bank(now, credits, expected);
        self.last_committed_type = Some(now);
        // Seam point 2 (§17.2): the typed key → v2's `Typed`, the only
        // event that builds its spine. The width is the same bound the credit
        // took: an unclamped saturated commit would lay an O(n²) fold through
        // the whole grid for a key v1 would refuse anyway.
        if self.v2.engaged() {
            self.v2.on_event(
                rk::Event::Typed {
                    cells: cells.min(Self::RAINBOW_TYPED_SWEEP_MAX as u16),
                    shifted,
                    class,
                },
                now,
            );
        }
    }

    /// The one-cell typed hint with the input glyph's exact identity. Hosts
    /// that know only a cell count use [`Self::note_typed_glyph`] and cannot
    /// enter the same-caret inference lane; every ordinary move is unchanged.
    pub fn note_typed_expected(
        &mut self,
        now: Instant,
        cells: u16,
        shifted: bool,
        class: rk::TypedClass,
        glyph: char,
    ) {
        self.note_typed_glyph_with_expected(
            now,
            cells,
            shifted,
            class,
            (cells == 1).then_some(glyph),
        );
    }

    /// Record a main-screen Enter classifier stamp. It licenses the following
    /// move, and a fresh stamp is the bare-Enter licence for the jump arm.
    ///
    /// This is the Enter that hands a line to a shell (or one the host cannot
    /// place), so the hand leaves its row ([`Self::hand_row`]); the next
    /// licensed typed echo names the new row. An Enter into a foreground
    /// program's composer is [`Self::note_return_keeping_hand`].
    pub fn note_return(&mut self, now: Instant) {
        self.note_return_keeping_hand(now);
        self.hand_row = None;
        self.unowned_caret = None;
    }

    /// [`Self::note_return`] for an Enter into a foreground program's composer
    /// (shell integration says a command is running, or the alternate screen
    /// is up): the same licence, but the hand keeps its row
    /// ([`Self::hand_row`]). Claude Code and Codex redraw their composer where
    /// the hand types next, so the first key after a submit stays guarded
    /// against a spinner or footer tick in its window.
    pub fn note_return_keeping_hand(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.flush_held_park();
        self.unsettle();
        self.user_gesture_hint = None;
        self.return_hint = Some(now);
        // Seam point 2 (§17.2): the Enter key → v2's `Return`, inert by
        // design (a keypress is not a move); the flight arrives as the
        // observed move with `Licence::Return`.
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::Return, now);
        }
    }

    /// Record a Tab / scripted-gesture classifier boundary. It licenses the
    /// following move within [`Self::USER_GESTURE_HINT_FRESH`]. A paste does
    /// not ride this class: its delivered insert arms
    /// [`Self::note_insert_delivered_from`] instead.
    ///
    /// A gesture goes through [`Self::supersede_typed_press`] and keeps the
    /// banked typed stamps (real keys whose echoes are in flight) and the
    /// Return, nav, quench and composer-newline one-shots, as a glyph does:
    /// an arrow's hop or Backspace's retreat in flight at the Tab is that
    /// key's echo, and the gesture waits for its completion. The classic
    /// trail's gesture still drops its nav hint.
    pub fn note_user_gesture(&mut self, now: Instant) {
        self.flush_held_park();
        self.supersede_typed_press();
        self.unsettle();
        self.user_gesture_hint = Some(now);
    }

    /// Host delivery edge for the key classes: a key queued behind a draining
    /// paste had its licence revoked at dispatch (`revoke_input_hints_at`),
    /// and its completed write re-stamps it here, one stamp for one echo. A
    /// pure re-stamp: the credit, v2 events, thermals and momentum went out at
    /// the key and were never revoked, and `note_motion`'s `clear_typed` would
    /// wipe stamps of glyphs delivered in the same batch. Erase classes read
    /// the pre-erase row fill from the previous probe
    /// ([`Self::row_fill_at_delivery`]). The delivered insert is
    /// [`Self::note_insert_delivered_from`].
    pub fn note_delivered(&mut self, at: Instant, class: DeliveredClass) {
        self.unsettle();
        match class {
            DeliveredClass::Typed => self.type_hint.stamp(at),
            DeliveredClass::Return => self.return_hint = Some(at),
            DeliveredClass::Newline => self.newline_hint = Some(at),
            DeliveredClass::Nav => self.nav_hint = Some(at),
            DeliveredClass::Gesture => self.user_gesture_hint = Some(at),
            DeliveredClass::Erase => {
                self.quench_hint = Some(at);
                self.bs_poof_hint = Some(at);
                self.bs_baseline = self.row_fill_at_delivery();
            }
            DeliveredClass::Kill { moves_cursor, word } => {
                self.kill_hint = Some(at);
                self.kill_hint_word = word;
                self.bs_baseline = self.row_fill_at_delivery();
                if moves_cursor {
                    self.nav_hint = Some(at);
                    self.kill_retreat_pending = Some(at);
                }
            }
        }
    }

    /// The row fill an erase key stamps as its pre-erase baseline
    /// ([`Self::bs_baseline`]): this frame's probe if landed, else the last
    /// presented truth.
    pub(super) fn row_fill_at_key(&self) -> Option<(u16, u16)> {
        self.row_cur_meta
            .or(self.row_prev_meta)
            .map(|m| (m.row, m.fill))
    }

    /// The row fill an erase delivery stamps as its baseline: the previous
    /// probe first, since receipts apply after this frame's
    /// `observe_row_with_trust` and `row_cur_meta` may already be post-erase.
    pub(super) fn row_fill_at_delivery(&self) -> Option<(u16, u16)> {
        self.row_prev_meta
            .or(self.row_cur_meta)
            .map(|m| (m.row, m.fill))
    }

    /// [`Self::note_insert_delivered_from`] for a test that has no dispatch
    /// instant to hand over (the retro claim then reaches back from `at`).
    #[cfg(test)]
    pub fn note_insert_delivered(&mut self, at: Instant, width: InsertWidth) {
        self.deliver_insert(at, None, width);
    }

    /// Host delivery edge: an insert's bytes (file drop, ⌘V, the `paste` verb,
    /// a Tab completion, a raw ⌃V) have provably landed on the wire (the FIFO
    /// writer's completed write, or the dispatch instant for a synchronous
    /// insert), `width` as priced by the host ([`InsertWidth`]). Arms the
    /// [`InsertLicence`] with the row witness read now ([`Self::insert_row`]):
    /// the one same-row forward echo that fits is laid as a single
    /// [`rk::Event::Sweep`] joining the live cohort (see the insert arm in
    /// [`Self::spawn`] and [`Self::echo_anchor_pass`]).
    ///
    /// A pure stamp: it supersedes nothing, survives class-changing supersedes
    /// and yields to a fresher class at the seam; every read is Rainbow
    /// Kitty-gated. A second delivery before the first is spent accumulates. A
    /// matching hop refused within [`Self::INSERT_RETRO_FRESH`], at or after
    /// `dispatched_at`, is retro-licensed ([`PendingHop`]); bytes cannot echo
    /// before dispatch. `Cells(0)` arms nothing.
    pub fn note_insert_delivered_from(
        &mut self,
        dispatched_at: Instant,
        at: Instant,
        width: InsertWidth,
    ) {
        self.deliver_insert(at, Some(dispatched_at), width);
    }

    pub(super) fn deliver_insert(
        &mut self,
        at: Instant,
        dispatched_at: Option<Instant>,
        width: InsertWidth,
    ) {
        let Some(armed) = self.arm_insert(at, dispatched_at.unwrap_or(at), width) else {
            return;
        };
        // Late receipt ([`PendingHop`]): the frame that observed the echo
        // already refused this insert's same-row forward hop inside the retro
        // window; lay it now on the delivery clock (priced by exact width,
        // unknown by the hand's row within its bound). The anchored lane's
        // brand on that row was this insert's echo and is lifted with it.
        if let Some(hop) = self.insert.take_retro_hop(armed, at, dispatched_at) {
            let ins = InsertLicence {
                row: Some(hop.row),
                ..armed
            };
            for entry in self.anchor_rows.iter_mut().flatten() {
                if entry.row == hop.row {
                    entry.tainted = false;
                }
            }
            self.lay_insert(hop.row, hop.col0, hop.row, hop.col1, ins, at);
        }
    }

    /// The dispatch-time arm: a bare Tab / ⌃V written inline arms the insert
    /// licence at dispatch, without the late-receipt claim. The retro window is
    /// symmetric in time, so at dispatch it would admit a hop refused before
    /// the press. A pending hop is left for a real receipt. The licence is
    /// stamped at `at`, so a queued Tab's revoke finds it and the delivery
    /// re-arm counts the gesture once and makes the retro claim.
    pub fn note_insert_armed(&mut self, at: Instant, width: InsertWidth) {
        let _ = self.arm_insert(at, at, width);
    }

    /// The arm shared by [`Self::deliver_insert`] and
    /// [`Self::note_insert_armed`]: resolve the width, read the row witness now
    /// (before the echo moves anything), and arm the seam
    /// ([`InsertSeam::arm`]). Returns the licence, or `None` for `Cells(0)`.
    pub(super) fn arm_insert(
        &mut self,
        at: Instant,
        dispatched_at: Instant,
        width: InsertWidth,
    ) -> Option<InsertLicence> {
        let (cells, known, breaks) = match width {
            InsertWidth::Cells(0) => return None,
            InsertWidth::Cells(cells) => (cells, true, false),
            InsertWidth::Line(0) | InsertWidth::Unknown => {
                (Self::INSERT_GESTURE_CELLS, false, false)
            }
            InsertWidth::Line(cells) => (cells, true, true),
        };
        self.recent_typed_run = None;
        self.unsettle();
        let row = self.insert_row(at);
        Some(
            self.insert
                .arm(at, dispatched_at, cells, known, breaks, row),
        )
    }

    /// The insert's row at `now`, read once as [`InsertLicence::row`] (not the
    /// typing row [`Self::hand_row`]):
    ///
    /// * a visible caret's row when it sits with the hand; it wins over the
    ///   licensed-move memory, since a program prompt can move the caret
    ///   without a licensed move;
    /// * under a hidden or parked caret, the row of the last licensed move
    ///   (typed or anchored echo, insert, rewrite) within the chain window,
    ///   else the established echo row ([`Self::last_anchor_sweep`]);
    /// * `None` when nothing can say.
    ///
    /// A caret is parked when it is visible on a row other than the anchored
    /// lane's established echo row (a TUI that CUPs to 1;1 while echoing
    /// through [`Self::echo_anchor_pass`]); trusting it would let a Tab spend
    /// on a status row. A caret merely differing from the print anchor (Codex:
    /// transcript printing above) or on the established row is never demoted.
    /// A caret shown on a fresh row since the lane last licensed elsewhere
    /// reads as parked until an echo names its row.
    pub(super) fn insert_row(&self, now: Instant) -> Option<u16> {
        let established = self.last_anchor_sweep.map(|(row, _)| row);
        if let Some((row, _)) = self.last
            && established.is_none_or(|er| er == row)
        {
            return Some(row);
        }
        self.last_licensed_row
            .filter(|(_, at)| {
                now.saturating_duration_since(*at).as_secs_f32() <= rk::ribbon::CHAIN_GAP_MAX
            })
            .map(|(row, _)| row)
            .or(established)
    }

    /// Remember a refused same-row forward hop `col0 → col1` on `row` for one
    /// in-flight delivery receipt ([`PendingHop`]); Rainbow Kitty only. Shared
    /// by `spawn_judged` and `echo_anchor_pass`.
    pub(super) fn remember_refused_hop(
        &mut self,
        row: u16,
        col0: u16,
        col1: u16,
        now: Instant,
        cfg: &GlowConfig,
    ) {
        if matches!(cfg.style, GlowStyle::RainbowKitty) {
            self.insert.pending_hop = Some(PendingHop {
                row,
                col0,
                col1,
                at: now,
            });
        }
    }

    /// Move the insert's row-addressed members with a scroll or band move
    /// ([`InsertSeam::translate_rows`]); the lane's own row witness names an
    /// absolute row too and is dropped with them.
    pub(super) fn translate_insert_rows(&mut self, map: &impl Fn(u16) -> Option<u16>) {
        self.insert.translate_rows(map);
        self.last_licensed_row = None;
    }

    /// How many cells the fresh delivered insert `ins` may buy on a same-row
    /// forward echo: its width plus the unpaid typed credits queued behind it.
    pub(super) fn insert_reach(&self, ins: InsertLicence, now: Instant) -> usize {
        usize::from(ins.cells) + self.typed_credits_within(now)
    }

    /// The insert echo shape: a same-row forward move no wider than the fresh
    /// insert's reach, under Rainbow Kitty. `None` otherwise, and the move is
    /// program output to this class.
    pub(super) fn insert_echo(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
    ) -> Option<InsertLicence> {
        if !matches!(cfg.style, GlowStyle::RainbowKitty) || cr != pr || cc <= pc {
            return None;
        }
        let ins = self.insert.fresh(now)?;
        let hop = usize::from(cc - pc);
        (hop <= self.insert_reach(ins, now)).then_some(ins)
    }

    /// Row identity for the insert. The stamp is only delivery correlation;
    /// spinners and timers advance inside its window too. A row may spend the
    /// insert when it is the row the hand was on at the arm
    /// ([`InsertLicence::row`]), or, for a priced width only, when the advance
    /// is the insert's whole width. An unknown-width insert (Tab, raw ⌃V,
    /// multi-line body, bounded at [`Self::INSERT_GESTURE_CELLS`]) is spent on
    /// the witnessed row alone: not a prompt a later Return printed, not a
    /// ⇧Tab mode line, not an untyped row.
    pub(super) fn insert_row_identity(row: u16, hop: usize, ins: InsertLicence) -> bool {
        let own_row = ins.row == Some(row);
        own_row || (ins.known && hop >= usize::from(ins.cells))
    }

    /// The insert's echo on the visible lane: the one predicate
    /// `spawn_judged`'s insert arm and the hide bridge
    /// ([`Self::hidden_bridge_source`]) share: the echo shape
    /// ([`Self::insert_echo`]) on the insert's own row
    /// ([`Self::insert_row_identity`]) with no fresher press class owning the
    /// hop ([`Self::fresher_class_owns_hop`]).
    pub(super) fn visible_insert_echo(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
    ) -> Option<InsertLicence> {
        let ins = self.insert_echo(pr, pc, cr, cc, now, cfg)?;
        let hop = usize::from(cc - pc);
        (Self::insert_row_identity(cr, hop, ins) && !self.fresher_class_owns_hop(hop, ins, now))
            .then_some(ins)
    }

    /// The line insert's echo across its break ([`InsertWidth::Line`]): the
    /// caret leaves the insert's own row for the row under it and lands at or
    /// left of where the line began (the new line's start: a composer's
    /// indent, a shell's column 0), the line's whole width fits on its row, no
    /// press dispatched before the insert is unpaid (its glyph would share the
    /// line), and no fresher press class owns the line
    /// ([`Self::fresher_class_owns_hop`]). Returns the licence and the column
    /// the line ends at; `None` otherwise, and the move is judged as any
    /// other. A row identity as strict as an unknown width's: the witnessed
    /// row alone.
    pub(super) fn line_insert_echo(
        &self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> Option<(InsertLicence, u16)> {
        if !matches!(cfg.style, GlowStyle::RainbowKitty) || pr.checked_add(1) != Some(cr) || cc > pc
        {
            return None;
        }
        let ins = self
            .insert
            .fresh(now)
            .filter(|ins| ins.known && ins.breaks && ins.row == Some(pr))?;
        let end = pc
            .checked_add(ins.cells)
            .filter(|&end| usize::from(end) < geom.cols)?;
        let ahead = self
            .type_press_ring
            .cells_where(now, |key_at| key_at <= ins.dispatched_at);
        (ahead == 0 && !self.fresher_class_owns_hop(usize::from(ins.cells), ins, now))
            .then_some((ins, end))
    }

    /// Whether a fresher press class owns this same-row forward hop, so the
    /// insert waits for its own echo: a navigation press, a reflow, a scripted
    /// gesture stamped at another instant, or unpaid typed presses dispatched
    /// before the insert whose credits cover the hop. Keys dispatched behind
    /// the insert cannot echo ahead of its bytes on the FIFO wire, and keep
    /// their credits.
    pub(super) fn fresher_class_owns_hop(
        &self,
        hop: usize,
        ins: InsertLicence,
        now: Instant,
    ) -> bool {
        if hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            || hint_fresh(self.reflow_hint, now, Self::REFLOW_HINT_FRESH)
            || (hint_fresh(self.user_gesture_hint, now, Self::USER_GESTURE_HINT_FRESH)
                && self.user_gesture_hint != Some(ins.at))
        {
            return true;
        }
        let credits = self
            .type_press_ring
            .cells_where(now, |key_at| key_at <= ins.dispatched_at);
        credits > 0 && credits >= hop
    }

    /// Whether a keyed erase class is fresh (Backspace, kill chord,
    /// navigation): a caret retreat under one is that key's own, never a
    /// program rewrite.
    pub(super) fn erase_key_fresh(&self, now: Instant) -> bool {
        hint_fresh(self.quench_hint, now, Self::QUENCH_HINT_FRESH)
            || hint_fresh(self.kill_hint, now, Self::KILL_HINT_FRESH)
            || hint_fresh(self.bs_poof_hint, now, Self::KILL_HINT_FRESH)
            || hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
    }

    /// Whether the row probe, when it speaks for `row` this frame, shows every
    /// cell at and right of `col` blank: the rewrite arm's content proof (no
    /// lit cells under blanks). A readline redraw caught between CR and reprint
    /// has the text still on the row and retracts nothing. With no probe for
    /// this row, geometry alone decides.
    pub(super) fn probe_blank_from(&self, row: u16, col: u16, now: Instant) -> bool {
        let Some(meta) = self.row_cur_meta else {
            return true;
        };
        if meta.row != row
            || now.saturating_duration_since(meta.at).as_secs_f32() > Self::POOF_PROBE_STALE
        {
            return true;
        }
        meta.fill <= col
    }

    /// The insert's rewrite shape: a keyless same-row retreat landing inside
    /// the span a delivered insert laid within [`Self::INSERT_REWRITE_FRESH`]
    /// (Claude Code swapping a dropped path for `[Image #1] `), with the row
    /// probe (when it speaks) showing the suffix blank. Returns the cells
    /// retreated; `None` otherwise.
    pub(super) fn insert_rewrite(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
    ) -> Option<u16> {
        if !matches!(cfg.style, GlowStyle::RainbowKitty) || cr != pr || cc >= pc {
            return None;
        }
        if !self.insert.span_admits_retreat(cr, cc, now)
            || self.erase_key_fresh(now)
            || !self.probe_blank_from(cr, cc, now)
        {
            return None;
        }
        Some(pc - cc)
    }

    /// Lay a delivered insert's echo `pc → cc` on `row`: take the one-shot, pay
    /// the surplus beyond the insert's width (keys typed behind the paste) from
    /// the press ring and consume their stamps, and hand v2 one
    /// [`rk::Event::Sweep`] over the span on the delivery clock behind an inert
    /// typed move, so the cells join the live cohort. Nothing else: no
    /// `classify_move`, thermals, momentum, click or v1 geometry; an insert is
    /// not typing. Logged `licensed licence=insert`.
    pub(super) fn lay_insert(
        &mut self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        ins: InsertLicence,
        now: Instant,
    ) {
        self.unsettle();
        if let Some(orphans) = self.insert.orphans.as_mut() {
            orphans.site = None;
        }
        let hop = usize::from(cc.saturating_sub(pc));
        let surplus = hop.saturating_sub(usize::from(ins.cells));
        if surplus > 0 {
            // Presses the surplus drains take their stamps with them, so a key
            // echoed inside the hop cannot license a later program advance.
            let credits = self.typed_credits_within(now);
            let presses = self
                .type_press_ring
                .spend_counting(now, surplus.min(credits));
            for _ in 0..presses {
                let _ = self.type_hint.take_fresh(now, Self::TYPE_HINT_FRESH);
            }
        }
        // An unknown width pays no surplus below its bound; presses banked at
        // or before its delivery are retired later ([`InsertSeam::orphans`]).
        if !ins.known && self.type_press_ring.cells_where(now, |t| t <= ins.at) > 0 {
            let site = self
                .row_cur_meta
                .filter(|m| m.row == cr && m.caret == cc && m.at == now)
                .and_then(|_| {
                    self.print_anchor
                        .filter(|&(row, col, _)| row == cr && col == cc)
                        .map(|(_, _, seq)| (cr, cc, seq))
                });
            self.insert.orphans = Some(UnknownInsertOrphans {
                dispatched_at: ins.dispatched_at,
                delivered_at: ins.at,
                laid_at: now,
                site,
            });
        }
        self.insert.laid(InsertSpan {
            row: cr,
            col0: pc,
            col1: cc,
            laid_at: now,
        });
        // A Tab / ⌃V also armed the gesture class at the same instant; once the
        // insert took its same-row hop, that one-shot is spent too.
        if self.user_gesture_hint == Some(ins.at) {
            self.user_gesture_hint = None;
        }
        self.last_licensed_row = Some((cr, now));
        // The insert's bytes landed at the caret: the hand's own input, as a
        // typed key's echo is.
        self.hand_lands_on(cr);
        if self.v2.engaged() {
            // Born on the delivery clock (the echoing frame shows the run lit,
            // no edge-in), floored like a key's sweep.
            self.v2.on_event(
                rk::Event::Sweep {
                    row: cr,
                    col0: pc,
                    col1: cc,
                },
                Self::sweep_born(ins.at, now),
            );
            self.v2.on_event(
                rk::Event::Move {
                    from: (pr, pc),
                    to: (cr, cc),
                    licence: rk::Licence::Insert,
                    dir: rk::Dir::of(i32::from(cc) - i32::from(pc), i32::from(cr) - i32::from(pr)),
                },
                now,
            );
        }
        self.spawns += 1;
        self.log_licensed_as(now, (pr, pc), (cr, cc), AdmissionRecord::LICENCE_INSERT);
    }

    /// Retract a delivered insert's span to the program's new caret: v2 gets
    /// [`rk::Event::Rewrite`] (the ribbon drains the suffix from `col`
    /// farthest-first at `12·n + 240` like a kill; nothing is born or sounds),
    /// the span memory shrinks to the caret, and the ring records
    /// `licensed licence=rewrite` from the old column `col + cells`.
    pub(super) fn retract_insert(&mut self, row: u16, col: u16, cells: u16, now: Instant) {
        self.unsettle();
        if self.v2.engaged() {
            self.v2
                .on_event(rk::Event::Rewrite { row, col, cells }, now);
        }
        self.insert.retracted(col);
        self.last_licensed_row = Some((row, now));
        self.spawns += 1;
        self.log_licensed_as(
            now,
            (row, col.saturating_add(cells)),
            (row, col),
            AdmissionRecord::LICENCE_REWRITE,
        );
    }

    /// `trail status`'s insert rows — see [`InsertTally`].
    #[must_use]
    pub fn insert_tally(&self) -> InsertTally {
        self.insert.tally
    }

    /// `trail status`'s in-flight rows ([`InFlightTally`]), read at the last
    /// tick's clock (`heat_at`).
    #[must_use]
    pub fn in_flight_tally(&self) -> InFlightTally {
        InFlightTally {
            credits: self.heat_at.map_or(0, |now| self.typed_credits_within(now)),
            ..self.in_flight_tally
        }
    }

    /// A composer newline landed: an alt-screen Shift+Enter, which agent
    /// composers bind to a line break without submitting (see
    /// [`Self::newline_hint`]).
    ///
    /// Armed only where the typed hint for that chord is armed: a main-screen
    /// Shift+Enter is Enter, a plain Enter takes [`Self::note_return`], and
    /// other modified chords arm nothing. It never gates bytes or counts as
    /// typing; it licenses the authored row change, and v2 receives the
    /// key-time `ComposerNewline` so its fresh-line walk is dated at the
    /// chord. The ribbon waits for the authored home move before a new-line
    /// glyph may spend that gate.
    pub fn note_newline_break(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.unsettle();
        self.newline_hint = Some(now);
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::ComposerNewline, now);
        }
    }

    /// Record a grid-geometry classifier boundary. Reflow itself is dark: the
    /// host resets this engine, since old cell-space paths are wrong after a
    /// resize, font or scale change.
    pub fn note_reflow(&mut self, now: Instant) {
        self.recent_typed_run = None;
        // Close any swallowed typed cohort first, so a resize cannot leave old
        // credits to pool with a later key.
        self.clear_typed(now);
        self.unsettle();
        self.reflow_hint = Some(now);
    }

    /// A user motion press: a key whose purpose is moving the cursor
    /// (navigation class, scroll chords like Ctrl-D). Stamps the same
    /// `nav_hint` as [`Self::note_navigation`]; the classifier reads the
    /// landing's shape to decide whether the jump choreography fires.
    pub fn note_motion(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.unowned_caret = None;
        // A motion moves the caret along the line editor: the hand keeps its
        // row ([`Self::clear_typed_keeping_hand`]).
        self.clear_typed_keeping_hand(now);
        self.unsettle();
        self.nav_hint = Some(now);
        // A real navigation press supersedes a kill's unpaid retreat: the move
        // this licenses is the user's word hop, not the kill's caret snap.
        self.kill_retreat_pending = None;
    }

    /// Explicit scripted preview/test provenance: stamps the gesture hint, so
    /// the scripted move is licensed exactly like a Tab/paste.
    #[doc(hidden)]
    pub fn note_synthetic_move(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.clear_typed(now);
        self.unsettle();
        self.user_gesture_hint = Some(now);
    }

    /// Scripted typed-move twin for previews, examples and benchmarks that
    /// deliberately exercise the typing classifier without a live terminal.
    #[doc(hidden)]
    pub fn note_synthetic_typed(&mut self, now: Instant, cells: u16) {
        self.note_typed_cells(now, cells);
    }

    /// Whether the bounded reflow classifier stamp is currently armed. This is
    /// observability for lifecycle tests, not an admission licence.
    #[must_use]
    pub fn reflow_hint_armed(&self) -> bool {
        self.reflow_hint.is_some()
    }

    /// Committed cell credits in flight (unpaid, inside
    /// [`IN_FLIGHT_PATIENCE_S`]): the press budget the anti-stray gates read
    /// (see [`Self::type_press_ring`]).
    pub(super) fn typed_credits_within(&self, now: Instant) -> usize {
        self.type_press_ring.cells_within(now)
    }

    /// The share rule: whether `paid` cells from `credits` unpaid presses buy a
    /// `cells`-wide typed hop: at least two presses, and at least three
    /// quarters of the swept cells. Mostly paid, not exactly: a burst whose
    /// ring dropped one press must still paint. The shape it refuses is one
    /// press echoing as a multi-cell hop (`w` over a five-cell word: 1 credit
    /// against 5). The two-press floor reads the raw ledger: one unechoed
    /// press is indistinguishable from a stale stamp.
    pub(super) fn share_rule(credits: usize, paid: usize, cells: usize) -> bool {
        credits >= 2 && paid * 4 >= cells * 3
    }

    /// Whether the unpaid presses pay for the same-row forward hop `pc → cc`
    /// on `row` under the share rule ([`Self::share_rule`]).
    ///
    /// A press pays for the cells its own glyph occupies. The host prices what
    /// it can see, but a program may echo a different glyph from the key (a
    /// terminal-side input method, full-width forms), banking one cell per
    /// press while the caret advances two. So the run's width comes from the
    /// grid: [`Self::swept_wide_continuations`] counts the `'\0'` continuation
    /// columns in the row probe. Each unpaid press may claim at most one (a
    /// glyph is at most two cells), so over an all-wide run the rule becomes
    /// `presses >= 0.75 * glyphs`, the narrow rule in glyph space.
    pub(super) fn typed_share_paid(&self, row: u16, pc: u16, cc: u16, now: Instant) -> bool {
        let credits = self.typed_credits_within(now);
        let paid = credits.saturating_add(
            self.swept_wide_continuations(row, pc, cc, now)
                .min(self.type_press_ring.presses_within(now)),
        );
        Self::share_rule(credits, paid, usize::from(cc.saturating_sub(pc)))
    }

    /// How many columns of the same-row run `[from_col, to_col)` are wide
    /// continuations (the second cell of a CJK or emoji glyph).
    ///
    /// Read from the host's row probe ([`Self::observe_row`]), captured under
    /// the same terminal lock as the move, which spells a continuation column
    /// `'\0'` (`Terminal::row_cols_into`). ZWJ sequences and complex clusters
    /// arrive as a resolved lead char plus `'\0'` and count once; combining
    /// marks advance no column.
    ///
    /// `0` whenever the probe cannot speak for this row (no probe, another
    /// row, or older than [`Self::POOF_PROBE_STALE`]). Both [`ProbeTrust`]
    /// classes are read: this is a content measurement and licenses nothing.
    pub(super) fn swept_wide_continuations(
        &self,
        row: u16,
        from_col: u16,
        to_col: u16,
        now: Instant,
    ) -> usize {
        let Some(meta) = self.row_cur_meta else {
            return 0;
        };
        if meta.row != row
            || now.saturating_duration_since(meta.at).as_secs_f32() > Self::POOF_PROBE_STALE
        {
            return 0;
        }
        let lo = usize::from(from_col);
        let hi = usize::from(to_col).min(self.row_cur.len());
        if lo >= hi {
            return 0;
        }
        self.row_cur[lo..hi]
            .iter()
            .filter(|&&ch| ch == '\0')
            .count()
    }

    /// Host key hint withheld: a plain typed glyph written to a tty in
    /// canonical no-echo mode (`read -s`, `sudo`, an `ssh` passphrase,
    /// `passwd`, read off the master's termios at the key), so it will never
    /// echo. Called instead of [`Self::note_typed_glyph`]: no stamp, credit,
    /// v2 `Typed` or supersede, so no later program write can spend it as a
    /// phantom. The only trace is `swallowed_no_echo=` on `trail status`.
    /// Raw-mode programs (Claude Code, vim, readline, `read -s -n`) echo for
    /// themselves and never come here.
    pub fn note_typed_swallowed_no_echo(&mut self) {
        self.in_flight_tally.swallowed_no_echo += 1;
    }

    /// Forget every in-flight press on an observed edge the presses cannot
    /// explain: a keyless backward or cross-row hop, a forward hop refused by
    /// the share rule (`no-credits`) or the cap, an unlicensed row change, a
    /// non-typed licence (Return, arrow, scripted gesture), a kill at the key.
    /// Mirrors the engine's erase/kill/focus ledger clears
    /// (`Engine::echo_bridge`), not its scroll and band clears: a
    /// scroll-translated echo keeps its host pool. So a swallowed press (a
    /// password, a pager's `q`) lives only until the next edge, not for
    /// [`IN_FLIGHT_PATIENCE_S`]. Counted as `inflight_forgotten=`.
    ///
    /// A refused same-row forward hop with no fresh stamp does not forget:
    /// its single credit is kept for a delivered insert's receipt
    /// ([`PendingHop`] / `insert_reach`) and the next key's ledger bridge.
    ///
    /// Counted only for a live press: dead slots past the patience are blanked
    /// too, but every licence read already treats them as empty.
    pub(super) fn forget_typed_credits(&mut self, now: Instant) {
        self.insert.orphans = None;
        self.insert.orphan_exact = None;
        if self.type_press_ring.is_empty() {
            return;
        }
        let live = self.type_press_ring.any_within(now);
        self.type_press_ring.forget();
        if live {
            self.in_flight_tally.forgotten += 1;
        }
    }

    /// Whether this move is a same-row forward echo with an unpaid press behind
    /// it: the licence of last resort for a repaint landing after every stamp
    /// went stale (see [`Self::spawn`] and `classify_move`). Rainbow Kitty
    /// only, since its ribbon records a cell per key.
    ///
    /// One-press echo: a single unpaid press licenses exactly its own +1 on its
    /// row, once. Wider hops keep the two-press floor and the share rule
    /// (`rainbow_coalesce`).
    ///
    /// Fold: the tight wrap shape ([`Self::fold_shape`]) is paid exactly, one
    /// press per cell laid (1..=3). The coalesced fold
    /// ([`Self::coalesced_fold_shape`]: a stalled batch crossing the wrap,
    /// landing at columns 1..=3) is admitted with exact payment
    /// (`credits == cells`); an overpaying batch is refused and forgotten.
    #[allow(
        clippy::too_many_arguments,
        reason = "from/to cursor cells + clock + config + geometry; the seam's one predicate, called from four sites"
    )]
    pub(super) fn unpaid_typed_echo(
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
        let credits = self.typed_credits_within(now);
        if cr == pr && cc > pc {
            // Exact for one cell, at least two for anything wider (the
            // coalesce's own floor).
            return credits >= usize::from(cc - pc).min(2);
        }
        // A composer can move a keyed word from mid-row to the next row after
        // the stamp expired; the exact erased tail + relocated tail + unpaid
        // glyph proof makes this move an echo. Generic cross-row hops still
        // cannot spend the pool.
        if credits >= 1 && self.carried_key_move == Some(((pr, pc), (cr, cc))) {
            return true;
        }
        if let Some(cells) = self.fold_shape(pr, pc, cr, cc, geom) {
            return cells >= 1 && credits >= cells;
        }
        self.coalesced_fold_shape(pr, pc, cr, cc, now, geom)
            .is_some_and(|cells| credits == cells)
    }

    /// The alt-screen blink discriminator: the host's repaint blink
    /// ([`Self::note_repaint_blink`], a DECTCEM hide inside a DEC 2026
    /// synchronized update) is younger than [`Self::BLINK_HINT_FRESH`]. It
    /// separates a full-redraw TUI's re-anchor and coalesce from a deliberate
    /// vim/less jump; the main screen never asks.
    pub(super) fn blink_fresh(&self, now: Instant) -> bool {
        hint_fresh(self.blink_hint, now, Self::BLINK_HINT_FRESH)
    }

    /// The focused pane's `(first column, width)` under the host's pane
    /// columns ([`Self::pane_columns`]), or the whole grid.
    pub(super) fn pane_span(&self, geom: Geom) -> (usize, usize) {
        self.pane_columns.map_or((0, geom.cols), |(col0, cols)| {
            (usize::from(col0), usize::from(cols))
        })
    }

    /// A move's pane-local columns for the fold shapes: `(pane_cols,
    /// pc_local, cc_local, pane_contains_move)` under the host's pane columns
    /// ([`Self::pane_columns`]) or the whole grid.
    pub(super) fn pane_local(&self, pc: u16, cc: u16, geom: Geom) -> (usize, usize, usize, bool) {
        let (pane_col0, pane_cols) = self.pane_span(geom);
        let pane_col1 = pane_col0.saturating_add(pane_cols);
        let pc_window = usize::from(pc);
        let cc_window = usize::from(cc);
        let pane_contains_move = pane_cols > 0
            && (pane_col0..pane_col1).contains(&pc_window)
            && (pane_col0..pane_col1).contains(&cc_window);
        (
            pane_cols,
            pc_window.saturating_sub(pane_col0),
            cc_window.saturating_sub(pane_col0),
            pane_contains_move,
        )
    }

    /// The soft-wrapped caret. Claude Code draws the key that fills row `r`'s
    /// last text column there and moves the caret alone to the continuation
    /// row's indent `(r + 1, 2)`; the next key reflows the partial word down.
    /// The coalesced fold and typed re-anchor would lay the landing row's head
    /// (`landing − 1`), which here is the blank indent.
    ///
    /// Read off the probes: a typed move exactly one row down whose landing
    /// row is blank from the pane's first column to the landing, while the
    /// origin cell `(pr, pc)`, blank in the last probe, now holds a glyph and
    /// every cell left of it is unchanged. Returns the first column of the
    /// word that glyph ends, or `None` off the shape. A Space leaves the origin
    /// blank and stays on the fold shapes.
    ///
    /// Or over the glyph it pushed on (`tests/wrap_code_reflow.rs`, B4): a key
    /// inserted into a token the box hard-breaks there (Ink's wrap-ansi
    /// `hard`) is drawn at the origin over the pushed glyph. The origin then
    /// changed to a glyph a fresh press stamped
    /// ([`PressCredits::fresh_glyph`]); an unchanged glyph (a shell's pending
    /// wrap) or an unstamped one proves nothing.
    pub(super) fn soft_wrapped_caret(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        pane_col0: usize,
        now: Instant,
    ) -> Option<u16> {
        let cur = self.row_cur_meta?;
        let prev = self.row_prev_meta?;
        if cr != pr.checked_add(1)?
            || usize::from(cc) <= pane_col0
            || cur.row != cr
            || cur.above != NbrProbe::Probed
            || prev.row != pr
            || prev.caret != pc
        {
            return None;
        }
        let blank = |row: &[char], col: usize| row.get(col).is_none_or(|&g| g == ' ');
        let origin = usize::from(pc);
        if !(pane_col0..usize::from(cc)).all(|col| blank(&self.row_cur, col))
            || blank(&self.row_above_cur, origin)
        {
            return None;
        }
        let glyph = self.row_above_cur[origin];
        if !blank(&self.row_prev, origin)
            && (self.row_prev[origin] == glyph
                || !self
                    .type_press_ring
                    .fresh_glyph(now, Self::TYPE_HINT_FRESH, glyph))
        {
            return None;
        }
        // Nothing left of the origin moved: the key went in at the fold, so
        // the origin row's prefix is unchanged. zle's Ctrl-R switching to an
        // older multi-line entry has the same shape but rewrites that prefix,
        // and must not be admitted.
        let at = |row: &[char], col: usize| row.get(col).copied().unwrap_or(' ');
        if (pane_col0..origin).any(|col| at(&self.row_prev, col) != at(&self.row_above_cur, col)) {
            return None;
        }
        let word_col0 = (pane_col0..origin)
            .rev()
            .take_while(|&col| !blank(&self.row_above_cur, col))
            .last()
            .unwrap_or(origin);
        u16::try_from(word_col0).ok()
    }

    /// Whether the forward echo to `(cr, cc)` from a soft-wrapped caret's
    /// landing carried the wrap key's word off its row
    /// (`tests/wrap_code_reflow.rs`, D): every column `sw.word_col0` to
    /// `sw.origin.1` is now blank in the probed row above. A burst or paste
    /// after a short word hops as far but leaves the word standing. `false`
    /// wherever the probe is not this move's or the row above was not probed.
    pub(super) fn carried_word_left(&self, sw: rk::ribbon::SoftWrap, cr: u16, cc: u16) -> bool {
        let Some(cur) = self.row_cur_meta else {
            return false;
        };
        if cur.row != cr
            || cur.caret != cc
            || cur.above != NbrProbe::Probed
            || sw.origin.0.checked_add(1) != Some(cr)
        {
            return false;
        }
        (sw.word_col0..=sw.origin.1).all(|col| {
            self.row_above_cur
                .get(usize::from(col))
                .is_none_or(|&g| g == ' ')
        })
    }

    /// The lifted word (the reverse re-wrap). A word typed at the start of the
    /// continuation row and glued to what follows (`already[Image #1]`) splits
    /// on the next Space and fits the row above again: Ink rewrites it at that
    /// row's end and the rest from the indent, with the caret moving backward
    /// on its row to the word's old first column. Such a move is never parked
    /// ([`Self::park_candidate`]), lays no landing, and hands the ribbon the
    /// verdict with its `Move` ([`rk::ribbon::Lift`]).
    ///
    /// Read off the probes: a same-row backward move whose span `cc..pc` held
    /// one word in the last probe and other text now, while the row above
    /// (probed both times) now ends with exactly that word after a blank where
    /// it was blank before. Returns the word's column on the row above, or
    /// `None` off the shape or with a probe missing.
    pub(super) fn lifted_word(&self, pr: u16, pc: u16, cr: u16, cc: u16) -> Option<u16> {
        let cur = self.row_cur_meta?;
        let prev = self.row_prev_meta?;
        if cr != pr
            || cc >= pc
            || cur.row != cr
            || cur.caret != cc
            || cur.above != NbrProbe::Probed
            || prev.row != pr
            || prev.caret != pc
            || prev.above != NbrProbe::Probed
        {
            return None;
        }
        let unit = rk::witness::unit_at;
        let word = cc..pc;
        if word.clone().any(|c| unit(&self.row_prev, c).is_blank())
            || word
                .clone()
                .all(|c| unit(&self.row_cur, c) == unit(&self.row_prev, c))
        {
            return None;
        }
        let end = u16::try_from(self.ink_end_in_pane(&self.row_above_cur)).ok()?;
        let dst = end.checked_sub(pc - cc)?;
        let before = dst.checked_sub(1)?;
        let arrived = word.clone().zip(dst..end).all(|(src, now)| {
            unit(&self.row_above_cur, now) == unit(&self.row_prev, src)
                && unit(&self.row_above_prev, now).is_blank()
        });
        (arrived && unit(&self.row_above_cur, before).is_blank()).then_some(dst)
    }

    /// The tight fold shape, shared by the classifier (`shape_wrap`'s first arm
    /// in `classify_move`) and the in-flight seam
    /// ([`Self::unpaid_typed_echo`]): down exactly one row, landing at the
    /// pane's first or second column, from its last or second-to-last column
    /// (a deferred-wrap echo lands at col 1; a wide glyph wraps from the
    /// second-to-last). Returns the cells the fold lays (origin tail from `pc`
    /// plus landing head before `cc`: 1..=3), or `None` off the shape. The tail
    /// is 0 when the pane's last cell was already paid as the pending-wrap
    /// print ([`Self::fold_origin_tail`], [`Self::wrap_paid_row`]).
    pub(super) fn fold_shape(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        geom: Geom,
    ) -> Option<usize> {
        let (pane_cols, pc_local, cc_local, pane_contains_move) = self.pane_local(pc, cc, geom);
        (cr == pr.saturating_add(1)
            && pane_contains_move
            && cc_local <= 1
            && pc_local.saturating_add(2) >= pane_cols)
            .then_some(
                self.fold_origin_tail(pr, pc_local, pane_cols)
                    .saturating_add(cc_local),
            )
    }

    /// The cells a fold lays on its origin row: `pc` to the pane's edge, unless
    /// the fold leaves the last column and the anchored lane already licensed
    /// that cell as the pending-wrap print ([`Self::wrap_paid_row`]). A
    /// last-column glyph arriving with the wrap prices both cells.
    pub(super) fn fold_origin_tail(&self, pr: u16, pc_local: usize, pane_cols: usize) -> usize {
        if pc_local.saturating_add(1) == pane_cols && self.wrap_paid_row == Some(pr) {
            0
        } else {
            pane_cols.saturating_sub(pc_local)
        }
    }

    /// The coalesced fold shape (`shape_wrap`'s second arm, shared with the
    /// in-flight seam): two or three glyphs delivered across the fold in one
    /// observed move. Down exactly one row, landing at pane columns 1..=3
    /// (so a plain Enter to column 0 only matches the tight arm), from within
    /// four columns of the pane's edge, and on the alt screen only under a
    /// fresh repaint blink (a blinkless cross-row hop there is vim's
    /// relocation). Returns the cells laid (2..=7) or `None`. The classifier
    /// admits it under a live typing rhythm or the in-flight licence; the seam
    /// admits it with exact payment.
    pub(super) fn coalesced_fold_shape(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        geom: Geom,
    ) -> Option<usize> {
        let (pane_cols, pc_local, cc_local, pane_contains_move) = self.pane_local(pc, cc, geom);
        (cr == pr.saturating_add(1)
            && pane_contains_move
            && (1..=3).contains(&cc_local)
            && pc_local.saturating_add(4) >= pane_cols
            && (!self.ctx_alt || self.blink_fresh(now)))
        .then_some(
            self.fold_origin_tail(pr, pc_local, pane_cols)
                .saturating_add(cc_local),
        )
    }

    /// Host key hint: one kill keypress (Ctrl-K/U/W, Alt-D, word-delete
    /// Backspace, forward Delete); a span of text is about to vanish.
    /// - The kill hint licenses the erase poof: within [`Self::KILL_HINT_FRESH`]
    ///   a same-row net shrink of the probed row puffs the style's poof off the
    ///   vanished span.
    /// - Fire's quench escalates like a big backspace
    ///   ([`Self::KILL_QUENCH_GAIN`]) and cools the heat and coal.
    /// - Moving-kill morphology retires older candidate classes; without exact
    ///   content/target proof its later caret relocation stays dark.
    ///
    /// `moves_cursor` says whether the kill normally relocates the caret.
    /// Ctrl-U/W and word-backspaces arm the bounded navigation classifier; a
    /// stationary kill (Ctrl-K, Alt-D, Delete) must not leak it into the next
    /// typed candidate.
    pub fn note_kill(&mut self, now: Instant, moves_cursor: bool) {
        self.recent_typed_run = None;
        self.flush_held_park();
        if self.newline_hint.take().is_some() && self.v2.engaged() {
            self.v2.on_event(rk::Event::CancelComposerNewline, now);
        }
        self.unsettle();
        // A kill removes the line's content: presses still in flight on it are
        // forgotten at the key, as the echo ledger does on its `Kill`.
        self.forget_typed_credits(now);
        // Word-Backspace reaches the generic Backspace arm first, then this
        // stronger classification at the same timestamp; the kill wins.
        self.bs_poof_hint = None;
        self.kill_hint = Some(now);
        self.quench = (self.quench + Self::KILL_QUENCH_GAIN).min(1.0);
        self.heat *= Self::QUENCH_COOL;
        self.coal *= Self::QUENCH_COOL;
        // The canonical metric un-earns about two deletes' worth; never builds.
        self.momentum.kill(now);
        // The erase mirror builds, and a span kill credits twice.
        self.erase_mom.advance(now);
        self.erase_mom.advance(now);
        // Stamp the pre-kill row as `note_backspace` does: both poof licences
        // share their downstream gates and so their witness. Read only by
        // `poof_erase_witnessed`; `bs_erased` cannot see a kill-stamped value,
        // since `note_backspace` re-stamps it before arming `bs_poof_hint`.
        self.bs_baseline = self.row_fill_at_key();
        // A caret-moving kill (Ctrl-U/W, word-backspace) is licensed by the nav
        // stamp armed here. A stationary kill moves no caret, so `spawn` never
        // runs and its poof comes from `tick`'s erase detector.
        if moves_cursor {
            self.nav_hint = Some(now);
        }
        // …and it owes exactly one such retreat (`kill_retreat_pending`).
        self.kill_retreat_pending = moves_cursor.then_some(now);
        // A fresh licence starts non-word; only `note_word_kill` marks it, after
        // this runs. `kill_reported_to_v2` is keyed on the stamp.
        self.kill_hint_word = false;
    }

    /// [`Self::note_kill`] for a word-scale kill chord (^W, Alt-D, Alt/Ctrl-
    /// Backspace): the same licence, but the poof speaks
    /// [`crate::trail_sound::SoundKind::KillWord`] instead of the swoosh.
    pub fn note_word_kill(&mut self, now: Instant, moves_cursor: bool) {
        self.note_kill(now, moves_cursor);
        self.kill_hint_word = true;
    }

    /// Disarm dangling typed and backspace hints when a key with its own move
    /// semantics arrives (Enter, Tab, nav, kill), so a hint left by a no-move
    /// echo (a password prompt, vim `x`/`r`) cannot re-anchor the next proven
    /// move. The GUI host calls it on every non-typed press and input boundary
    /// (mouse, focus, paste drain) before class-specific code arms; a typed
    /// press goes through [`Self::supersede_typed_press`], which keeps the
    /// banked stamps.
    ///
    /// It also takes the hand off its row ([`Self::hand_row`]): an unclassified
    /// press may open a minibuffer (zle's Ctrl-R/S, Alt-X, Escape into vi's
    /// `/`) or hand the keyboard to another program. Line-editing keys and bare
    /// modifier edges use [`Self::clear_typed_keeping_hand`].
    pub fn clear_typed(&mut self, now: Instant) {
        self.clear_typed_keeping_hand(now);
        // The hand did something other than type or edit its line.
        self.hand_row = None;
        self.unowned_caret = self
            .last
            .map(|(row, _)| UnownedCaret { row, parked: false });
    }

    /// A licensed echo of the hand's own input landed on `row`: a typed key's
    /// ([`Self::spawn_judged`]) or a delivered insert's ([`Self::lay_insert`]).
    /// The hand types there now ([`Self::hand_row`]), and a generic boundary's
    /// doubt about that row ([`Self::unowned_caret`]) is settled: the row took
    /// the hand's input, so a park observed after the hand moves on to another
    /// row (a composer's next line, then Backspace back) cannot disown it. A
    /// doubt about another row stands.
    pub(super) fn hand_lands_on(&mut self, row: u16) {
        self.hand_row = Some(HandRow::at(row));
        if self.unowned_caret.is_some_and(|caret| caret.row == row) {
            self.unowned_caret = None;
        }
    }

    /// [`Self::clear_typed`] for a press that cannot take the hand off its
    /// row: a line-editing press (Backspace, Delete, arrows, Home/End, word
    /// and line motions, kills), a paste (its delivered insert licenses its own
    /// echo), or a bare modifier edge. Every licence term closes the same way,
    /// but the hand keeps its row ([`Self::hand_row`]): none of these opens a
    /// minibuffer in zle, readline or an agent composer.
    pub fn clear_typed_keeping_hand(&mut self, now: Instant) {
        // Closes movement licences, including the boundary between two
        // controller `turn` calls. The recent typed run is content evidence,
        // not a licence: it is retired where edits, navigation and geometry
        // boundaries are classified.
        self.flush_held_park();
        if self.newline_hint.is_some() && self.v2.engaged() {
            self.v2.on_event(rk::Event::CancelComposerNewline, now);
        }
        self.type_hint.clear();
        self.clear_non_typed_hints(now);
    }

    /// The non-typed half of [`Self::clear_typed`]: every one-shot class,
    /// closed by a class-changing press. [`Self::supersede_typed_press`]
    /// closes less, since a glyph contradicts no one-shot whose echo is owed.
    pub(super) fn clear_non_typed_hints(&mut self, now: Instant) {
        self.user_gesture_hint = None;
        self.nav_hint = None;
        self.return_hint = None;
        self.reflow_hint = None;
        self.newline_hint = None;
        // The quench hint drops only once its own echo had about two frames to
        // land, so a fast backspace→Enter keeps the deletion steam.
        if self
            .quench_hint
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() > 0.03)
        {
            self.quench_hint = None;
        }
    }

    /// The typed-press supersede, [`Self::clear_typed`]'s sibling for a typed
    /// glyph: it closes only what a glyph contradicts (the Tab / scripted
    /// gesture and the reflow). The banked typed stamps survive, and so do the
    /// Return, nav, quench and composer-newline one-shots: a glyph typed before
    /// their echo lands (a slow prompt, a TUI's input latency) is type-ahead,
    /// and their late echo must not be judged under the glyph's stamp. The
    /// host calls this instead of `clear_typed` on the printable-glyph arm
    /// (and ahead of `note_return` / `note_user_gesture` for Enter, Tab and
    /// ⌃V); [`Self::note_typed_glyph`] calls it before banking its stamp.
    pub fn supersede_typed_press(&mut self) {
        self.user_gesture_hint = None;
        self.reflow_hint = None;
    }

    /// Revoke the classifier cohort armed by the input dispatch at `at`,
    /// licence terms included, so a key that never reached the child cannot
    /// license a later program move. Called when a key was queued behind a
    /// draining paste, an inline write failed, or a paste's arrival stamp must
    /// not outlive its enqueue (the completed write re-arms the delivered
    /// insert, [`Self::note_insert_delivered_from`]). Matching by timestamp
    /// avoids cancelling a newer gesture or that later delivery stamp.
    pub fn revoke_input_hints_at(&mut self, at: Instant) {
        let clear = |slot: &mut Option<Instant>| {
            if *slot == Some(at) {
                *slot = None;
            }
        };
        self.type_hint.revoke_at(at);
        // The press credit stays: a key queued behind a paste will reach the
        // wire, so its echo is payable by the pool and its stamp is re-banked
        // at the write's completion ([`Self::note_delivered`]). A failed write
        // is [`Self::revoke_failed_input_at`], which takes the credit too.
        clear(&mut self.quench_hint);
        clear(&mut self.nav_hint);
        clear(&mut self.return_hint);
        clear(&mut self.user_gesture_hint);
        clear(&mut self.newline_hint);
        clear(&mut self.reflow_hint);
        clear(&mut self.kill_hint);
        // The delivered insert is revoked by its exact instant too; an earlier
        // unspent insert this arm accumulated into is restored and the arm's
        // tally undone (a revoked arm was not a delivery).
        self.insert.revoke_arm_at(at);
        let revoke_bs = self.bs_poof_hint == Some(at);
        clear(&mut self.bs_poof_hint);
        if revoke_bs {
            self.bs_baseline = None;
        }
        if self.last_committed_type == Some(at) {
            self.last_committed_type = None;
        }
    }

    /// Revoke the cohort of a dispatch whose inline write failed (the child
    /// never saw the key): [`Self::revoke_input_hints_at`] plus the press
    /// credit, which alone would license the next program `+1` as `inflight`.
    /// A merely queued key keeps its credit, since it will echo.
    pub fn revoke_failed_input_at(&mut self, at: Instant) {
        self.revoke_input_hints_at(at);
        self.type_press_ring.revoke_at(at);
    }

    /// Consume every one-shot classifier at a completed hidden→visible boundary
    /// where `spawn` did not, including a same-cell return, so the hide
    /// choreography cannot license a later PTY/CUP move.
    ///
    /// Spared while fresh: a TUI that brackets each repaint in a DECTCEM hide
    /// completes hidden boundaries mid-burst, so typed stamps younger than
    /// [`Self::TYPE_HINT_FRESH`] survive (the delivered insert and a held park
    /// follow the same law; the credit ring keeps its own
    /// [`IN_FLIGHT_PATIENCE_S`] clock). The other one-shots clear, except a nav
    /// hint younger than [`Self::NAV_HINT_FRESH`]: a same-cell completion under
    /// it is a key whose echo has not landed (a viewport snap-back before zsh's
    /// `\x08`×n, an Ink repaint's hidden half before its `CUP`).
    pub(super) fn retire_hidden_movement_provenance(&mut self, now: Instant) {
        // A held park is spared while fresh, like the typed bank: Ink brackets
        // its park and rewrite in DECTCEM hides, so a boundary can complete
        // mid-park. A park past its window is judged now, as the tick would.
        if self.held_park.is_some_and(|p| !p.fresh(now)) {
            self.flush_held_park();
        }
        self.type_hint.retire_stale(now, Self::TYPE_HINT_FRESH);
        // The delivered insert follows the bank's law: spared while fresh,
        // retired once stale.
        if self.insert.fresh(now).is_none() {
            self.insert.clear();
        }
        self.quench_hint = None;
        if !hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH) {
            self.nav_hint = None;
        }
        self.return_hint = None;
        self.user_gesture_hint = None;
        self.newline_hint = None;
        self.reflow_hint = None;
        // The credit ring keeps its own clock: a credit retires when its cells
        // are laid or past the in-flight patience, never at `TYPE_HINT_FRESH`,
        // so a boundary seconds into a stall spares the whole batch.
        self.type_press_ring.retire_stale(now);
        if self
            .last_committed_type
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() > Self::TYPE_HINT_FRESH)
        {
            self.last_committed_type = None;
        }
    }

    /// The unconditional twin of [`Self::retire_hidden_movement_provenance`]
    /// for the unseeded first-draw boundary: a fresh or reset engine has no
    /// truthful source cell, so even a fresh stamp must not license the next
    /// CUP. Everything goes, bank and ring included.
    pub(super) fn retire_all_movement_provenance(&mut self) {
        self.flush_held_park();
        self.type_hint.clear();
        self.insert.clear();
        self.insert.pending_hop = None;
        self.insert.orphans = None;
        self.insert.orphan_exact = None;
        self.quench_hint = None;
        self.nav_hint = None;
        self.return_hint = None;
        self.user_gesture_hint = None;
        self.newline_hint = None;
        self.reflow_hint = None;
        self.type_press_ring.forget();
        self.last_committed_type = None;
    }

    /// A hidden→visible relocation the bounded ConPTY bridge declined never
    /// reaches `spawn`, so its one-shot classifiers are consumed here. For
    /// Rainbow Kitty the caret mirror moves to `cur`
    /// ([`rk::Engine::observe_caret`]) so the next key lays at the true cell;
    /// nothing of the ribbon is retired here: the content witness decides per
    /// cell on this frame (replaced or moved with the caret → fast melt; gone →
    /// the swoosh; intact → nothing, so the contrail fades).
    pub(super) fn retire_declined_hidden_relocation(&mut self, cur: (u16, u16)) {
        // Full consumption, not the fresh-sparing retire: the relocation was
        // refused, so nothing about it correlates with the banked keys, and a
        // spared stamp would be borrowed by the next one-cell program update
        // (`cold_program_motion_emits_no_rainbow_light`).
        self.retire_all_movement_provenance();
        // The caret crossed to a row the prior row sample did not observe:
        // retire kill/Backspace proofs and both probe generations so a reused
        // row number cannot synthesize a later poof.
        self.drop_row_probe();
        self.crown_until = None;
        if self.v2.engaged() {
            self.v2.observe_caret(cur);
        }
    }
}
