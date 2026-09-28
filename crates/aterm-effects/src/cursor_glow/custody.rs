// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TYPED INPUT CUSTODY — who typed what, and what it may still pay for: the
//! typed stamps, the press-credit ledger, the insert seam, and the host's
//! `note_*` input API that feeds them.

use super::*;

/// Whether a one-shot class hint stamped at `hint` is still inside its
/// `window` (seconds) at `now` — the one freshness predicate every licence
/// read asks of a stamp (`nav_hint`, `quench_hint`, `return_hint`, …); an
/// unstamped hint is never fresh. Saturating, so a stamp from the future
/// (a host clock handed in ahead of the tick's) reads as fresh, not stale.
#[inline]
pub(crate) fn hint_fresh(hint: Option<Instant>, now: Instant, window: f32) -> bool {
    hint.is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() <= window)
}

/// Spend a one-shot hint: take it only while it is fresh ([`hint_fresh`]),
/// leaving a stale stamp in place — the one consume every licence read
/// asks of a stamp (one hint, one echo).
#[inline]
pub(crate) fn take_hint_fresh(
    hint: &mut Option<Instant>,
    now: Instant,
    window: f32,
) -> Option<Instant> {
    hint.take_if(|t| now.saturating_duration_since(*t).as_secs_f32() <= window)
}

/// A bounded, exact content witness for the last adjacent typed echo run.
/// It is inert while the hand rests: only a later licensed typed echo reads
/// it. A retired ribbon cell cannot be held by its old cohort, but the next
/// key may re-lay the unchanged text it was typed beside.
pub(super) const RECENT_TYPED_RUN_CAP: usize = 256;

#[derive(Clone, Copy)]
pub(super) struct RecentTypedRun {
    pub(super) row: u16,
    pub(super) col0: u16,
    pub(super) len: u16,
    pub(super) glyphs: [char; RECENT_TYPED_RUN_CAP],
    pub(super) at: Instant,
}

impl RecentTypedRun {
    pub(super) fn end(&self) -> u16 {
        self.col0.saturating_add(self.len)
    }
}

/// Bounded FIFO of typed-press LICENCE stamps, packed oldest-first: K
/// presses bank K stamps (the oldest dropped past [`TYPED_STAMP_DEPTH`]),
/// an observed echo sweep pops exactly one — K keys license K sweeps, and
/// program output beyond what keys paid for finds an empty queue — a stamp
/// is a licence only within `TYPE_HINT_FRESH` of its own press and goes
/// stale IN PLACE (never withdrawn by a read), and under Rainbow Kitty a
/// stamp licenses only while the press ring still owes a cell
/// ([`CursorGlow::seam_licensed`]: a coalesced echo spends K presses and
/// pops one stamp, so the K-1 it leaves fresh license nothing). A 1-deep
/// slot collapsed K keys inside one frame gap into one stamp, and every
/// sweep after the first was declined into a permanent background-black
/// hole — measured on glass at 5-13 declines per 100-key flood, dark%
/// 11.5-13.2.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypedStamps {
    /// Packed oldest-first: every `Some` precedes every `None`.
    pub(super) slots: [Option<Instant>; TYPED_STAMP_DEPTH],
}

impl Default for TypedStamps {
    // Written out because `[T; N]: Default` stops at N = 32 and the bank is
    // deeper than that now (see [`TYPED_STAMP_DEPTH`]).
    fn default() -> Self {
        Self {
            slots: [None; TYPED_STAMP_DEPTH],
        }
    }
}

/// Depth of [`TypedStamps`] and [`PressCredits`] — equal to the
/// coalesced-sweep cell cap ([`CursorGlow::RAINBOW_TYPED_SWEEP_MAX`]): one
/// observed move can spend at most this many cells, so banking more presses
/// than that cannot license anything a move could ever sweep. A real TUI
/// batches by its own REPAINT, not by frames (a hand at 10 keys/s into a
/// 900 ms debounced input box hands the engine one observed move of 9-10
/// columns; the owner's measured 36 keys/s, 25), and a press is in flight
/// for [`IN_FLIGHT_PATIENCE_S`], so the banks hold what that patience
/// admits at a fast hand: 10 s at the measured sustained cadence (85 ms per
/// key, 11.8 keys/s) is 118 presses — 128 is the power of two above it, and
/// more than a full 100-column row, the widest same-row hop a wrap-less
/// line on the measured instance can produce (a 36 keys/s burst fills it
/// in 3.5 s; bursts do not last ten seconds). Past the depth the OLDEST
/// presses are dropped, and a batch deeper than the ring is refused by the
/// sweep cap (a re-anchor that lays only its landing) and forgotten — dark,
/// never wrong; on a terminal wider than 128 columns a 129+-cell same-row
/// echo is that shape, recorded. Cost: three 128-slot `Copy` arrays (≈ 9 KB
/// per `CursorGlow`) and a spend that is one O(depth) pass from the ring's
/// head, on the spawn edge only; the depth is past `[T; N]: Default`'s
/// limit, so the banks write their `Default` out by hand. `pub`: the host's
/// Backspace price memory (`app_input.rs`'s `ErasePriceMemory`) is bounded
/// by this depth too, by name.
pub const TYPED_STAMP_DEPTH: usize = 128;

/// **THE IN-FLIGHT PATIENCE** (seconds): the honest upper bound
/// on how long a typed press waits for its row to echo before it is
/// forgotten by the CLOCK. It is a BOUND, not the mechanism — every real
/// edge forgets the presses sooner (a keyless backward or cross-row hop, a
/// forward hop the share rule refuses, a row change no key licensed, a
/// Return, an arrow, a kill, focus loss, reset). It bounds a press whose row
/// has stayed SILENT, so it must cover the measured stall regime, not the
/// measured echo latency: real Claude Code 2.1.268 under three Rust
/// compiles stalled 2.7 s for the screenshot, and the matrix's longest real
/// stall was 7.7 s (hold-N30-D5; the 28 s hold-N12-D3 row was a harness
/// hiccup, excluded). 10 s is that worst case with ~30 % margin, twice the
/// ribbon's 5 s chain window (`ribbon::CHAIN_GAP_MAX`), and about the
/// longest a hand keeps typing blind into a frozen prompt. Not 5 s: the
/// matrix has a 7.7 s stall that was still the same sentence. One patience
/// for one press, whichever layer is asked: the press ring
/// ([`PressCredits`]) reads it by name and the engine's
/// `rainbow_kitty::ECHO_PATIENCE_S` is this number by alias.
pub(crate) const IN_FLIGHT_PATIENCE_S: f32 = 10.0;

/// Capacity of [`CursorGlow::anchor_rows`] — how many distinct rows' print
/// endpoints the anchored-echo lane remembers at once. A TUI interleaves the
/// input row with a spinner/status row (fake_claude repaints both); four rows
/// of memory keeps the input row's launch column alive across those
/// interleaves without growing a map.
pub(super) const ANCHOR_ROWS: usize = 4;

/// One entry of [`CursorGlow::anchor_rows`]: where the last print run on
/// `row` ended, plus the ROW-DISCRIMINATION brand the anchored-echo lane
/// judges against. `tainted` marks a PROGRAM row — one observed advancing
/// its end while no user gesture was fresh (a spinner, an elapsed timer, a
/// token counter: they advance keylessly all day, while the input row only
/// ever advances with keys). The brand is sticky for the entry's lifetime:
/// once a row has advanced without a human, its later advances must never
/// consume a banked typed stamp, even when they happen to land inside a
/// concurrent keystroke's freshness window (the refuter's stray — a 150 ms
/// status cadence under 90 ms typing kept every status advance stamp-fresh,
/// and each one lit and SPENT an input echo's stamp). One row is never
/// branded at all: the ESTABLISHED ECHO ROW — the current
/// [`CursorGlow::last_anchor_sweep`] holder — whose identity a licensed
/// anchored echo already proved, and which an UNDELIVERED paste (one whose
/// bytes never provably landed — the host revokes the arrival stamp when
/// enqueue is not delivery) advances keylessly (branding it there would
/// kill the anchor for the coordinate space's lifetime). A DELIVERED
/// insert's echo is exempt too, by the insert's own row identity
/// ([`CursorGlow::insert_row_identity`]). Cleared
/// with the entry on reset/scroll, where the coordinate space itself is
/// torn down.
#[derive(Clone, Copy)]
pub(super) struct AnchorRow {
    pub(super) row: u16,
    pub(super) end: u16,
    pub(super) tainted: bool,
}

impl TypedStamps {
    /// Bank one press. When full, the OLDEST stamp is dropped — the newest
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

    /// Take every stamp banked AFTER `at` out of the bank, keeping the rest
    /// packed — a held park is judged at its own clock against the bank as
    /// it stood then ([`CursorGlow::flush_park`]): a freshness read from a
    /// past `now` sees a later stamp as fresh, so a key typed after the
    /// park would be spent by its flush. Put back with [`Self::merge`].
    /// Written by index, like [`Self::retire_stale`] — one linear pass, not
    /// a per-stamp free-slot scan: a held park flushes on every keystroke
    /// of an Ink TUI, so this runs per key under a stall. No ordering is
    /// assumed.
    pub(crate) fn split_off_after(&mut self, at: Instant) -> Self {
        let mut kept = Self::default();
        let mut later = Self::default();
        let (mut nk, mut nl) = (0, 0);
        for t in self.slots.into_iter().flatten() {
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

    /// Put back what [`Self::split_off_after`] took: the later stamps go
    /// behind whatever the judgment left of the earlier ones, so the bank
    /// stays packed oldest-first. One `copy_within` and one slice copy —
    /// the newest [`TYPED_STAMP_DEPTH`] stamps survive exactly as banking
    /// them one by one through [`Self::stamp`] would leave them, without
    /// that loop's per-stamp free-slot scan.
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
        for t in self.slots.into_iter().flatten() {
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

    /// Whether ANY stamp is banked, fresh or stale — the observability the
    /// single slot's `is_some()` gave ("an unechoed stamp is not withdrawn —
    /// it goes stale in place").
    #[cfg(test)]
    pub(crate) fn armed(&self) -> bool {
        self.slots[0].is_some()
    }

    /// Whether any banked stamp is still within `window` of `now` — the
    /// license read (peek, never consumes).
    pub(crate) fn any_fresh(&self, now: Instant, window: f32) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|t| now.saturating_duration_since(*t).as_secs_f32() <= window)
    }

    /// Drop only the stamps OLDER than `window` — the hidden→visible boundary
    /// retirement. A TUI's repaint choreography (DECTCEM-hide inside a
    /// DEC-2026 bracket, per keystroke) completes a hidden boundary while the
    /// user is mid-burst; wiping the whole bank there orphaned every echo
    /// still in flight and printed permanent unlit ribbon cells. Stamps still
    /// within their own freshness window are REAL keys whose echoes have not
    /// arrived — they survive; everything older is consumed dark. Packing
    /// (every `Some` precedes every `None`) is preserved: stamps are banked
    /// oldest-first, so the stale prefix is removed and the fresh suffix
    /// shifts down.
    pub(crate) fn retire_stale(&mut self, now: Instant, window: f32) {
        self.retain(|t| now.saturating_duration_since(t).as_secs_f32() <= window);
    }

    /// The OLDEST still-fresh stamp, left in place — what [`Self::take_fresh`]
    /// would consume. The classifier peeks first and consumes only once the
    /// echo's shape says the stamp's own press is in it (a stalled batch
    /// older than the stamp leaves it for its key's own echo).
    pub(crate) fn peek_fresh(&self, now: Instant, window: f32) -> Option<Instant> {
        self.slots
            .iter()
            .flatten()
            .copied()
            .find(|t| now.saturating_duration_since(*t).as_secs_f32() <= window)
    }

    /// Consume the OLDEST still-fresh stamp (one stamp, one echo sweep —
    /// keys license echoes in press order). Stale stamps are left in place.
    pub(crate) fn take_fresh(&mut self, now: Instant, window: f32) -> Option<Instant> {
        let idx = self.slots.iter().position(|slot| {
            slot.is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() <= window)
        })?;
        let taken = self.slots[idx];
        self.slots.copy_within(idx + 1.., idx);
        self.slots[TYPED_STAMP_DEPTH - 1] = None;
        taken
    }
}

/// **THE PRESS-CREDIT RING** — the typed-echo coalescer's ledger, one slot
/// per keyed glyph (`(press instant, unpaid cells, exact glyph if known)`), banked by
/// [`CursorGlow::note_typed_glyph`] and spent, oldest first, by the cells a
/// licensed typed echo lays. THE LEDGER, NOT THE CLOCK: the presses that
/// produce a late batch are by construction older than the batch (a hand at
/// 10 keys/s into a 700 ms debounced repaint offers seven cells backed by
/// presses 0.1–0.7 s old, of which a 0.5 s window measured from the
/// observation saw four — one short of the share rule, the sweep refused,
/// six typed cells never born; measured on glass at that frontier: a
/// 6-column hop painted and a 7-column hop tore in the same second), so
/// the honest question is not how OLD a press is but whether its cells
/// have been LAID. A slot holds a press whose glyph is still IN FLIGHT: it
/// leaves the ring when its cells are laid, when an observed edge the press
/// cannot explain forgets it ([`Self::forget`],
/// [`CursorGlow::forget_typed_credits`]), when the key that erases it is
/// pressed ([`Self::retire_newest`]), or — the bound — when it is older
/// than [`IN_FLIGHT_PATIENCE_S`]: a press is live for that one patience,
/// and every read is life-filtered ([`Self::within`]). A scroll or a
/// row-band move keeps the pool (the geometry moves and the anchors drop,
/// never the ring): a stalled batch whose box grew before it echoed is the
/// echo the pool is there to pay for. Fixed-size and `Copy`: the steady
/// frame path allocates nothing.
pub(super) type LiveUnpaidPress = (usize, Instant, u8, Option<char>);

/// A loaded host can miss a whole composer row, not just the first four
/// letters in the captured take. Bound this exceptional exact-glyph proof by
/// the same 128 credits the ordinary typed sweep can spend. Up to eight older
/// credits may precede the run, but none may be skipped inside it.
pub(super) const EXACT_COALESCED_PREFIX_MAX: usize = TYPED_STAMP_DEPTH;
pub(super) const EXACT_COALESCED_STALE_PREFIX_MAX: usize = 8;
/// At most two queued keys can be proved after an unknown insert, one cell
/// per frame or as one exact two-cell row transition. Longer runs remain
/// ambiguous about which keys the insert's hop already echoed.
pub(super) const ORPHAN_EXACT_KEYS_MAX: usize = 2;
/// Fixed, press-ordered escrow candidates recovered after an unknown insert.
pub(super) type OrphanExactPressRun = [Option<(Instant, char)>; ORPHAN_EXACT_KEYS_MAX];

#[derive(Clone, Copy, Debug)]
pub(crate) struct PressCredits {
    pub(super) slots: [Option<(Instant, u8, Option<char>)>; TYPED_STAMP_DEPTH],
    /// The next slot to write — the ring overwrites its OLDEST press when
    /// full, so the newest [`TYPED_STAMP_DEPTH`] presses are the ones kept.
    /// Only [`Self::bank`] moves it; every other mutator blanks slots in
    /// place, so walking `head, head + 1, …` (mod the depth) visits the
    /// live presses oldest-first — the order [`Self::spend_counting`]
    /// spends in.
    pub(super) head: usize,
}

impl Default for PressCredits {
    // Written out because `[T; N]: Default` stops at N = 32.
    fn default() -> Self {
        Self {
            slots: [None; TYPED_STAMP_DEPTH],
            head: 0,
        }
    }
}

impl PressCredits {
    /// Bank one press worth `credits` cells.
    pub(super) fn bank(&mut self, now: Instant, credits: u8, glyph: Option<char>) {
        self.slots[self.head] = Some((now, credits, glyph));
        self.head = (self.head + 1) % TYPED_STAMP_DEPTH;
    }

    /// The LIVE presses: unpaid, and younger than [`IN_FLIGHT_PATIENCE_S`].
    pub(super) fn within(
        &self,
        now: Instant,
    ) -> impl Iterator<Item = (Instant, u8, Option<char>)> + '_ {
        self.slots
            .iter()
            .flatten()
            .copied()
            .filter(move |(t, c, _)| {
                *c > 0 && now.saturating_duration_since(*t).as_secs_f32() <= IN_FLIGHT_PATIENCE_S
            })
    }

    /// Unpaid CELLS of the live presses whose instant `keep` admits — the
    /// press BUDGET the anti-stray gates read, partitioned by the key clock
    /// the caller names (strictly before a stamp: the presses whose glyphs
    /// lie to the LEFT of that key's own cell; at or after it: the key's own
    /// press and the ones behind it; at or before a park: the presses in
    /// flight when it was observed, the only ones its return may be funded
    /// from).
    pub(super) fn cells_where(&self, now: Instant, keep: impl Fn(Instant) -> bool) -> usize {
        self.within(now)
            .filter(|(t, _, _)| keep(*t))
            .map(|(_, c, _)| usize::from(c))
            .sum()
    }

    /// Unpaid CELLS of every live press.
    pub(super) fn cells_within(&self, now: Instant) -> usize {
        self.cells_where(now, |_| true)
    }

    /// Unpaid PRESSES — the same pool counted in keys.
    pub(super) fn presses_within(&self, now: Instant) -> usize {
        self.within(now).count()
    }

    /// The OLDEST unpaid press, or `None` when the pool is empty — the typed
    /// licence of last resort for a same-row forward echo whose repaint
    /// landed after every banked stamp went stale (`classify_move`).
    /// Oldest, because presses license echoes in press order — the rule
    /// [`TypedStamps::take_fresh`] follows.
    pub(super) fn oldest_unpaid(&self, now: Instant) -> Option<Instant> {
        self.within(now).map(|(t, _, _)| t).min()
    }

    /// Whether an unpaid press stamped exactly `glyph` within `freshness_s`
    /// — the key's own echo, read off the glass. Older unspent credits can
    /// survive a TUI's earlier suppressed echoes for the full in-flight
    /// patience; as for [`Self::oldest_fresh_glyph`], they are not
    /// witnesses for this new print. ANY fresh press, not only the oldest:
    /// the glyph a coalesced echo leaves beside its caret is its LAST key's.
    /// A press with no stamped glyph (a host that knows only a cell count)
    /// witnesses nothing.
    pub(super) fn fresh_glyph(&self, now: Instant, freshness_s: f32, glyph: char) -> bool {
        self.within(now).any(|(at, _, g)| {
            g == Some(glyph) && now.saturating_duration_since(at).as_secs_f32() <= freshness_s
        })
    }

    /// The oldest FRESH press's exact glyph, in ring order. Older unspent
    /// credits can survive a TUI's earlier suppressed echoes for the full
    /// in-flight patience; they are not witnesses for this new print. A
    /// later typeahead key cannot replace the earlier fresh glyph.
    pub(super) fn oldest_fresh_glyph(
        &self,
        now: Instant,
        freshness_s: f32,
    ) -> Option<(usize, char)> {
        for k in 0..TYPED_STAMP_DEPTH {
            let i = (self.head + k) % TYPED_STAMP_DEPTH;
            if let Some((at, credits, glyph)) = self.slots[i]
                && credits > 0
                && now.saturating_duration_since(at).as_secs_f32() <= freshness_s
            {
                return glyph.map(|glyph| (i, glyph));
            }
        }
        None
    }

    /// Whether `printed` is FOREIGN to every press in flight: `Some(true)`
    /// when at least one press is live, every live press carries its exact
    /// glyph ([`CursorGlow::note_typed_expected`]) and none of them is
    /// `printed`; `Some(false)` when one of them is. `None` — unknown — for
    /// an empty pool or any glyph-less live press (a wide glyph, an IME
    /// commit, a host or test that banks only a cell count): nothing may be
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

    /// The chronological unpaid one-cell keys must spell this entire small
    /// observed run. A stalled compositor may first show several Codex echoes
    /// in one frame after the oldest key exceeds the short hint window; the
    /// in-flight ledger, bounded to its normal patience, still remembers the
    /// exact sequence. Only credits already stale at the prior blank-row
    /// sample may be skipped before the run; none may be skipped inside it.
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

    /// The same chronological proof with a caller-selected tail age. A
    /// same-caret blank-to-glyph transition itself witnesses a single delayed
    /// echo; a moving coalesced prefix still requires its final key to be
    /// fresh. Earlier credits may be skipped only when they predate the prior
    /// row probe and have aged beyond the short key hint.
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
            if credits == 0
                || now.saturating_duration_since(at).as_secs_f32() > IN_FLIGHT_PATIENCE_S
            {
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
            // older than the sampled blank row and the short hint window.
            // That includes a repeated old `a`; a valid delayed echo above
            // still wins before this skip is considered.
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

    /// A later exact content echo proves earlier same-stream keys already
    /// appeared or were suppressed. Drop their unpaid credits before the
    /// existing oldest-first spend, so a suppressed echo cannot later fund
    /// unrelated program output. Identify the selected slot by ring position,
    /// not timestamp: two keys can share the host's input clock.
    pub(super) fn retire_before_slot(&mut self, selected: usize) {
        for k in 0..TYPED_STAMP_DEPTH {
            let i = (self.head + k) % TYPED_STAMP_DEPTH;
            if i == selected {
                return;
            }
            self.slots[i] = None;
        }
    }

    /// SPEND `cells`, oldest-first: the presses that produced this echo are
    /// the ones consumed, so one pool of real typing can never fund a second
    /// echo. Returns how many presses the spend drained to zero.
    ///
    /// One pass, chronological from [`Self::head`]: `bank` is the only
    /// writer that moves the head, and every other mutator only blanks
    /// slots, so walking `head, head + 1, …` visits the live presses
    /// oldest-first without a per-press rescan for the minimum.
    pub(super) fn spend_counting(&mut self, now: Instant, mut cells: usize) -> usize {
        let mut drained = 0;
        for k in 0..TYPED_STAMP_DEPTH {
            if cells == 0 {
                break;
            }
            let i = (self.head + k) % TYPED_STAMP_DEPTH;
            let Some((t, c, _)) = &mut self.slots[i] else {
                continue;
            };
            if *c == 0 || now.saturating_duration_since(*t).as_secs_f32() > IN_FLIGHT_PATIENCE_S {
                continue;
            }
            let take = usize::from(*c).min(cells);
            *c -= take as u8;
            cells -= take;
            if *c == 0 {
                self.slots[i] = None;
                drained += 1;
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

    /// REVOKE the press banked at `at` — the revoke contract for a dispatch
    /// that never reached the child (a failed inline write, a key queued
    /// behind a draining paste): a press the tty never saw is neither a
    /// licence nor a credit, and a credit alone would license the next
    /// program `+1` (the one-press echo).
    pub(super) fn revoke_at(&mut self, at: Instant) {
        self.blank_where(|t| t == at);
    }

    /// FORGET every press — an observed edge the presses cannot explain.
    pub(super) fn forget(&mut self) {
        self.slots = [None; TYPED_STAMP_DEPTH];
        self.head = 0;
    }

    /// RETIRE every press banked AT OR BEFORE `at` — the twin of
    /// [`Self::revoke_at`] for the presses an unknown-width insert's hop
    /// echoed without pricing them ([`CursorGlow::insert_orphans`]): a
    /// press on the wire ahead of or behind the insert's bytes can only
    /// echo in that hop or the program's next repaint, so past that it is
    /// bounded away rather than left to license a keyless `+1` for the
    /// patience. Presses banked after `at` are untouched.
    pub(super) fn retire_through(&mut self, at: Instant) {
        self.blank_where(|t| t <= at);
    }

    /// One or two exact one-cell keys dispatched after an insert but banked
    /// by its delivery, in press order. Their later exact blank-to-glyph
    /// echoes may prove them singly or as one complete two-cell transition;
    /// a wider or non-exact run remains ambiguous about which keys the
    /// unknown insert's hop already echoed.
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
            if at <= dispatched_at
                || at > delivered_at
                || credits == 0
                || now.saturating_duration_since(at).as_secs_f32() > IN_FLIGHT_PATIENCE_S
            {
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

    /// Take every press banked AFTER `at` out of the ring, at its own index
    /// — the twin of [`TypedStamps::split_off_after`] for a held park's
    /// flush: the judgment then spends, forgets or reads only
    /// the presses in flight at the park. The head is copied so
    /// [`Self::merge`] can walk the taken presses oldest-first.
    pub(super) fn split_off_after(&mut self, at: Instant) -> Self {
        let mut later = Self {
            slots: [None; TYPED_STAMP_DEPTH],
            head: self.head,
        };
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_some_and(|(t, _, _)| t > at) {
                later.slots[i] = slot.take();
            }
        }
        later
    }

    /// Put back what [`Self::split_off_after`] took. The judgment never
    /// banks (only `bank` moves the head), so when it left the ring in
    /// place the presses go back to their own slots; when it FORGOT the
    /// ring (head reset, every slot blank) they are re-banked oldest-first
    /// from the copied head, so the chronological walk still holds.
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

    /// The slot of the NEWEST press, if any.
    pub(super) fn newest(&self) -> Option<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.map(|(t, _, _)| (i, t)))
            .max_by_key(|&(_, t)| t)
            .map(|(i, _)| i)
    }

    /// Retire the NEWEST press — the key a Backspace erases. A wide press
    /// goes whole: one key laid one glyph, however many cells it was priced
    /// at.
    pub(super) fn retire_newest(&mut self) {
        if let Some(i) = self.newest() {
            self.slots[i] = None;
        }
    }

    /// Retire `cells` of the NEWEST press — one glyph out of a press that
    /// banked several. A committed IME run is ONE press priced at its
    /// summed width ([`CursorGlow::note_typed_cells`]), and a Backspace
    /// erases one glyph of it, not the run: the slot pays back that glyph's
    /// cells and goes only when its last cell is paid, so the run's
    /// surviving glyphs keep the credits their echo will spend. An erase
    /// wider than the slot still holds takes the slot and no more — the
    /// glyph belonged to that press, never to the one before it. An erase
    /// of ZERO cells takes nothing: a zero-width cluster the run carried
    /// (a base-less combining mark, a stray selector) advanced no cell
    /// and pays none back, and the press stands — in particular the press
    /// BEFORE the run, which is the newest once the run is paid out and
    /// which a whole-press retire would have taken for a glyph it never
    /// laid.
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

/// A queued KEY's licence class, re-stamped at its delivery
/// ([`CursorGlow::note_delivered`]): what the key banked at dispatch and
/// the enqueue revoked. The delivered INSERT is not a key class — its
/// witness is the completed write itself ([`InsertWidth`],
/// [`CursorGlow::note_insert_delivered_from`]).
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
    /// delivered insert the same receipt carries — the cross-row completion
    /// is the gesture's, the same-row sweep the insert's.
    Gesture,
}

/// HOW THE HOST PRICED A DELIVERED INSERT ([`CursorGlow::note_insert_delivered`]).
/// The two classes are spent under different row laws, so the distinction
/// travels as a type rather than a sentinel width: a 32-cell paste is a
/// PRICED insert and must never inherit the unknown class's terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertWidth {
    /// Priced in cells from the very text put on the wire
    /// (`Terminal::paste_insert_cells`). `Cells(0)` arms nothing.
    Cells(u16),
    /// The text cannot say — a Tab's completion (one `\t` byte; the shell
    /// decides), a raw ⌃V (the app's own placeholder), a multi-line or
    /// tabbed body, a body past the price probe. Bounded at
    /// [`CursorGlow::INSERT_GESTURE_CELLS`], and spendable ONLY on the row
    /// the hand was on when it was armed.
    Unknown,
}

/// **THE DELIVERED-INSERT LICENCE**: a human insert gesture — a file drop,
/// ⌘V, the control `paste`
/// verb, an in-app ⌃V image insert, a Tab completion — whose bytes have
/// PROVABLY landed on the wire, priced in cells by the host. It is the one
/// licence class whose witness is DELIVERY rather than a keypress: to the
/// PTY stream a paste's echo and a program flood are the same bytes, so the
/// only honest discriminator is the host's completed write. One-shot (taken
/// by the sweep it licenses), fresh for [`CursorGlow::INSERT_HINT_FRESH`],
/// and spendable ONLY by the echo SHAPE — a same-row forward advance no
/// wider than `cells` plus the unpaid typed credits queued behind it, on
/// the row that is the insert's own ([`CursorGlow::insert_row_identity`]).
/// See [`CursorGlow::note_insert_delivered`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InsertLicence {
    /// The delivery instant — the writer thread's completed write (or the
    /// dispatch instant for a synchronous insert such as Tab).
    pub(super) at: Instant,
    /// The first byte of this insert was dispatched before keys pressed after
    /// this instant. Those keys cannot own an echo of this insert's bytes,
    /// even if their credits are already banked when the frame arrives.
    pub(super) dispatched_at: Instant,
    /// The insert's cell width as the host priced it from the text it put on
    /// the wire, or the [`CursorGlow::INSERT_GESTURE_CELLS`] bound when it
    /// could not. Accumulates when a second insert is delivered before the
    /// first was spent (a multi-file drop is one `Paste` per file).
    pub(super) cells: u16,
    /// Whether `cells` is the PRICED width ([`InsertWidth::Cells`]) or the
    /// unknown class's bound. An accumulation is priced only when every
    /// part of it was.
    pub(super) known: bool,
    /// THE ROW WITNESS: the row the hand was on when the licence was armed
    /// ([`CursorGlow::hand_row`] — the visible caret's row unless the
    /// anchored lane established the echo row elsewhere, else the row of
    /// the last licensed move within the ribbon's chain window, else that
    /// established row), `None` when nothing could say (a hidden caret on a
    /// never-typed row). The stamp
    /// is delivery CORRELATION; this is the identity an insert of unknown
    /// width has nothing else to offer, and it is what keeps a Tab's stamp
    /// off the prompt a later Return printed on another row, and a ⇧Tab's
    /// mode line from spending anything at all.
    pub(super) row: Option<u16>,
}

/// Where a delivered insert's sweep was laid — the REWRITE window's memory.
/// A TUI that swaps the inserted text for a placeholder (Claude Code turning
/// a dropped image path into `[Image #1] `) pulls the caret BACK inside this
/// span with no key behind it; the seam reads that keyless retreat as the
/// insert's own rewrite and retracts the ribbon to the new caret
/// ([`rk::Event::Rewrite`]), never leaving lit cells under blanks. Row-
/// addressed like [`AnchorRow`]: dropped with the anchor memory on a scroll,
/// a row-band move and reset, stale after
/// [`CursorGlow::INSERT_REWRITE_FRESH`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InsertSpan {
    pub(super) row: u16,
    pub(super) col0: u16,
    pub(super) col1: u16,
    pub(super) laid_at: Instant,
}

/// A same-row FORWARD hop the seam refused for want of a licence, remembered
/// for one delivery: the writer thread publishes the insert's receipt
/// microseconds after the write returns, but a preempted writer can publish
/// AFTER the frame that observed the echo, and both lanes re-seed their
/// origin on that frame — so a receipt that lands late would license nothing
/// and the whole insert would be dark, not one frame of it. When
/// [`CursorGlow::note_insert_delivered`] finds a hop exactly the insert's
/// width refused within [`CursorGlow::INSERT_RETRO_FRESH`], it lays that hop
/// (the echo ledger's own late-echo shape) — the ACCUMULATED width when the
/// receipt lands on a still-armed one; an insert of UNKNOWN width has
/// no width to match and claims the hop by its ROW WITNESS instead — the
/// hop on the row the hand was on, no wider than
/// [`CursorGlow::INSERT_GESTURE_CELLS`]. A hop of any other width (or, for
/// the unknown class, on any other row) is not the insert's and is
/// forgotten. Row-addressed: dropped with the anchor memory on a scroll, a
/// row-band move and reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PendingHop {
    pub(super) row: u16,
    pub(super) col0: u16,
    pub(super) col1: u16,
    pub(super) at: Instant,
}

/// **THE PARK** — Ink's rewrite observed as two sync brackets: Ink repaints
/// the input row by parking the caret LEFT (a same-row backward move), then
/// rewriting the row to its end plus the new glyph, and a frame can catch
/// the two ~40 ms apart. Judged at once, the park is a typed re-anchor
/// (the key's stamp spent on it, the mirror moved to the park column so the
/// key's `Typed` replay lays the PROMPT cell) and the return is refused for
/// want of a licence: half the row dark. So a same-row backward
/// typed-paired (or in-flight-backed, or delivered-insert-backed) move is
/// HELD, not judged — nothing consumed, nothing spent, no v2 `Move`. It is
/// RELEASED by its return on either lane ([`CursorGlow::release_held_park`]:
/// a forward move within one stamp window from the landing past the
/// origin, reclassified as `origin -> target`; or the rewrite's print
/// advancing the row's end from the origin under a hidden caret) or
/// CANCELLED by a same-end rewrite (a repaint that carried no glyph), and
/// FLUSHED at its own clock by every other edge — through the ordinary
/// body, so every other verdict (the box-growth re-anchor, a ConPTY
/// hide-bridged retreat) is byte-identical, only emitted one observed move
/// (or ≤ 0.25 s) later. It rides a scroll or a band move with its row
/// ([`CursorGlow::carry_held_park`]) and is a WAKE SOURCE: `is_active`, and
/// a deadline just past its window in `next_change_deadline`, so a host
/// that sleeps on the engine's word still delivers a silent park's verdict
/// on time. Rainbow Kitty only; `Copy` (the config and geometry it must be
/// judged under ride along, so any edge can flush it), no allocation.
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
    /// Whether a GLYPH sat at the landing cell (`landing - 1`) when the
    /// park was held — read from the host's row probe at park time. A
    /// flushed park is judged as a typed re-anchor, and the re-anchor's
    /// landing sweep exists for the box-growth wrap, whose landing cell IS
    /// the wrapped glyph; for an Ink park to the input's start the cell
    /// left of the landing is the PROMPT, and a rewrite arriving after the
    /// window must not light it with no keystroke behind it. Unknown (no
    /// probe, or one for another row) reads as a glyph, so every
    /// direct-drive path is unchanged.
    pub(super) landing_glyph: bool,
    /// WHICH glyph sat at the landing cell (`landing - 1`) when the park was
    /// held — THIS frame's probe of the landing row
    /// ([`CursorGlow::probe_glyph_before`]), `None` where it is unknown. The
    /// flush judges the park at its own clock, where no probe is that
    /// frame's, so this is the landing gate's witness there
    /// ([`MoveCtx::landing_foreign`]): zsh's Ctrl-R walking the match BACK
    /// along its row (`l`, then `lo` — the caret retreats from `world`'s `l`
    /// to `hello`'s `lo`) is held as Ink's park, and its flush laid the cell
    /// under `hello`'s first `l`, which no key typed.
    pub(super) landing_cell: Option<char>,
    /// The content witness carried this exact park's source row away.
    pub(super) content_followed: bool,
}

impl HeldPark {
    pub(super) fn fresh(&self, now: Instant) -> bool {
        let seconds = if self.cross_row {
            IN_FLIGHT_PATIENCE_S
        } else {
            CursorGlow::TYPE_HINT_FRESH
        };
        now.saturating_duration_since(self.at).as_secs_f32() <= seconds
    }
}

/// `trail status`'s delivered-insert rows: how many inserts the host
/// delivered, how many the seam lit, how many placeholder rewrites it
/// retracted, and the last insert's priced width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InsertTally {
    /// Inserts the host reported delivered (`note_insert_delivered`), less
    /// any arm revoked at its exact instant before it was a delivery (a Tab
    /// queued behind a draining paste is counted once, at its re-arm).
    pub delivered: u64,
    /// Inserts whose echo the seam licensed and laid as one sweep.
    pub lit: u64,
    /// Program rewrites of a lit span the seam retracted.
    pub retracted: u64,
    /// The last delivered insert's cell width — of the last arm that
    /// STOOD: an arm revoked at its instant puts back the width before it,
    /// as it puts back the `delivered` tick.
    pub last_cells: u16,
}

/// **THE DELIVERED-INSERT SEAM** — one mechanism: a delivery-witnessed
/// licence with a row identity ([`InsertLicence`]), the undo a revoked arm
/// restores ([`InsertArmUndo`]), the span the licence was laid as
/// ([`InsertSpan`]), the refused hop one delivery may retro-license
/// ([`PendingHop`]), the presses an unknown width's hop echoed, and `trail
/// status`'s tally. Its invariants are this impl's: the undo is written only
/// at an arm; the span and the hop are row-addressed and dropped with the
/// row space; the orphans are cleared with the press pool. The licence
/// survives the class-changing supersedes (`clear_typed`, a modifier
/// release, a nav press) — its witness is the delivery, not a key, and a
/// debounced TUI echoes it up to ~1.4 s later — and yields to a fresher
/// class at the seam instead ([`CursorGlow::fresher_class_owns_hop`]);
/// revoked only by exact instant, a coordinate-space teardown, or its own
/// expiry.
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
    /// THE PRESSES AN UNKNOWN-WIDTH INSERT'S HOP ECHOED: `(the insert's
    /// delivery instant, the tick that laid its hop)`. An unknown insert (a
    /// multi-line paste, ⌃V, Tab) is priced at its
    /// [`CursorGlow::INSERT_GESTURE_CELLS`] bound, so a hop narrower than
    /// that pays no surplus, and the typed presses queued behind the insert
    /// (`[Pasted text #1 +3 lines] k` echoing as one 17-cell hop) would keep
    /// their credits for the whole patience and license the next keyless
    /// `+1` on the row as `inflight`. Bound, don't spend: the presses banked
    /// at or before the delivery were on the wire ahead of or immediately
    /// behind the insert's bytes and can only echo in its hop or the
    /// program's next repaint, so they are retired
    /// ([`PressCredits::retire_through`]) once the hop is
    /// [`CursorGlow::INSERT_REWRITE_FRESH`] old ([`Self::retire_orphans`]).
    /// A key whose echo lands the next frame spends its own credit as `key`
    /// before that, and the retire is then a no-op. Cleared with the pool.
    pub(super) orphans: Option<UnknownInsertOrphans>,
    /// Up to two ambiguous post-insert keys, removed from the generic press
    /// pool at orphan cleanup. Only their ordered exact glyphs at consecutive
    /// cells can claim them, singly or as one exact two-cell frame. Every
    /// other movement discards the remaining escrow and leaves the ordinary
    /// pool empty.
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

/// What an arm overwrote, restored when THAT arm is revoked at its exact
/// instant: a Tab or ⌃V is armed at dispatch and revoked when its write was
/// queued behind a draining paste, and the revoke must not take the paste's
/// credit down with the Tab's — a revoked arm was not a delivery, so the
/// tally reads as it did before it.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct InsertArmUndo {
    /// The still-fresh, unspent licence the newest arm ACCUMULATED into.
    /// Dropped with the armed licence everywhere else.
    pub(super) licence: Option<InsertLicence>,
    /// `last_insert_cells=` as it read before the newest arm. Overwritten by
    /// every arm and never cleared: read by the revoke of the newest arm
    /// only.
    pub(super) last_cells: u16,
}

impl InsertSeam {
    /// ARM a delivered insert of `cells` (`known` when the host priced them,
    /// else the unknown class's bound) at `at`, `row` the hand's row read
    /// now ([`CursorGlow::hand_row`]). A second delivery before the first
    /// was spent ACCUMULATES: winit hands a multi-file drop over as one
    /// `Paste` per file, both drain on the one writer thread inside a
    /// frame, and the shell echoes `p1 p2 ` as ONE hop the sum wide. The
    /// sum is priced only if both parts were; the newer arm's row witness
    /// wins when it has one; a stale stamp is simply replaced. The
    /// accumulated-into licence is kept as the undo, so a revoke of THIS
    /// arm restores it rather than discarding the paste's credit.
    pub(super) fn arm(
        &mut self,
        at: Instant,
        dispatched_at: Instant,
        cells: u16,
        known: bool,
        row: Option<u16>,
    ) -> InsertLicence {
        self.tally.delivered += 1;
        self.undo.last_cells = self.tally.last_cells;
        self.tally.last_cells = cells;
        let prior = self.armed.filter(|prev| {
            at.saturating_duration_since(prev.at).as_secs_f32() <= CursorGlow::INSERT_HINT_FRESH
        });
        let armed = match prior {
            Some(prev) => InsertLicence {
                at,
                dispatched_at: prev.dispatched_at.min(dispatched_at),
                // A PRICED part adds its exact width; the unknown class's
                // bound is counted ONCE per licence — a second unspent Tab
                // / ⌃V (an auto-repeat, a double-tap that beeped) does not
                // widen it: summing the bound grew it 32 cells per repeat,
                // so after three Tabs a 68-cell program hop on the hand's
                // row was lit as the insert and a held Tab bought ~1900
                // cells.
                cells: if known || prev.known {
                    prev.cells.saturating_add(cells)
                } else {
                    prev.cells
                },
                known: prev.known && known,
                row: row.or(prev.row),
            },
            None => InsertLicence {
                at,
                dispatched_at,
                cells,
                known,
                row,
            },
        };
        self.undo.licence = prior;
        self.armed = Some(armed);
        armed
    }

    /// REVOKE the arm stamped at exactly `at` (a dispatch that was queued
    /// or failed): the licence it accumulated into is restored, its
    /// `delivered` tick undone and `last_cells` put back — the re-arm at
    /// delivery counts the gesture once; a failed write never re-arms and
    /// counts nothing. An arm at any other instant is untouched.
    pub(super) fn revoke_arm_at(&mut self, at: Instant) {
        if self.armed.is_some_and(|i| i.at == at) {
            self.armed = self.undo.licence.take();
            self.tally.delivered = self.tally.delivered.saturating_sub(1);
            self.tally.last_cells = self.undo.last_cells;
        }
    }

    /// The armed licence while it is fresh — [`CursorGlow::INSERT_HINT_FRESH`]
    /// for a PRICED insert, the much shorter
    /// [`CursorGlow::INSERT_GESTURE_HINT_FRESH`] for the unknown-width class.
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

    /// A coordinate-space teardown: every licence term goes — the licence,
    /// its undo, the span, the hop. The tally and the undo width are
    /// readings, not licences, and stay.
    pub(super) fn teardown(&mut self) {
        self.clear();
        self.span = None;
        self.pending_hop = None;
        self.orphans = None;
        self.orphan_exact = None;
    }

    /// Move the ROW-ADDRESSED members with a scroll or a band move under
    /// `map` (an absolute row to its new row, or `None` once carried off the
    /// grid / past the band's edge): the span memory and the pending hop
    /// name absolute rows and are dropped outright, like the anchor memory;
    /// the licence's row witness (and the credit it accumulated into) rides
    /// `map`, so a priced width is still recognised by its whole width once
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

    /// TAKE the refused hop remembered for one delivery ([`PendingHop`]) if
    /// it is the insert `armed` now delivers — refused within
    /// [`CursorGlow::INSERT_RETRO_FRESH`] of `at`, at or after
    /// `dispatched_at` when the receipt knows it (the key's bytes could not
    /// have echoed before they were on their way), and the insert's by
    /// width or by row witness ([`Self::retro_hop_is_the_inserts`]).
    /// Matched against the ACCUMULATED licence, not this receipt alone: two
    /// receipts straddling the echoing frame (a multi-file drop whose second
    /// receipt a preempted writer publishes after the frame) echo as ONE
    /// hop the sum wide. The hop is forgotten either way: one delivery, one
    /// claim.
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

    /// Whether a refused hop is the insert now delivered: a PRICED width by
    /// exactly its width; an UNKNOWN width by the ROW WITNESS alone,
    /// bounded at [`CursorGlow::INSERT_GESTURE_CELLS`] (the same terms
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
    /// insert laid within [`CursorGlow::INSERT_REWRITE_FRESH`] — the
    /// rewrite window's shape half; the erase-key and row-probe reads are
    /// the lane's ([`CursorGlow::insert_rewrite`]).
    pub(super) fn span_admits_retreat(&self, row: u16, col: u16, now: Instant) -> bool {
        self.span.is_some_and(|span| {
            span.row == row
                && col >= span.col0
                && col <= span.col1
                && now.saturating_duration_since(span.laid_at).as_secs_f32()
                    <= CursorGlow::INSERT_REWRITE_FRESH
        })
    }

    /// The licence was LAID as `span`: the one-shot is taken with the credit
    /// it accumulated into, the refused hop is moot, the span opens the
    /// rewrite window, and `trail status` counts the lit insert.
    pub(super) fn laid(&mut self, span: InsertSpan) {
        self.clear();
        self.pending_hop = None;
        self.span = Some(span);
        self.orphan_exact = None;
        self.tally.lit += 1;
    }

    /// The laid span was RETRACTED to the program's new caret `col`: the
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

    /// The presses an unknown insert's hop echoed are bounded away once the
    /// hop is old enough for any next-frame echo of theirs to have spent
    /// them ([`Self::orphans`]): returns the delivery instant to retire the
    /// press ring through, once.
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

/// `trail status`'s IN-FLIGHT rows (the stall) —
/// `inflight_licensed=` / `inflight_forgotten=` / `credits=` /
/// `swallowed_no_echo=` / `park_returns=` / `park_flushed=`. The reading that
/// says whether a stalled batch reached the seam and how it was judged: a batch
/// licensed by the in-flight pool alone (no stamp fresh — the unpaid-press
/// licence) is otherwise indistinguishable in the ring from a fresh-stamp
/// coalesce, a forget is otherwise silent, a press the host withheld because
/// the tty would never echo it is otherwise invisible, and a held park's fate —
/// returned inside the window, or flushed at its own clock — is otherwise
/// unreadable. See [`CursorGlow::in_flight_tally`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InFlightTally {
    /// Moves licensed by the in-flight pool with no stamp fresh (the ring
    /// row reads `licence=inflight`).
    pub licensed: u64,
    /// Times an observed edge forgot a pool with a live press in it
    /// ([`CursorGlow::forget_typed_credits`] — the seam's forget edges; a
    /// teardown's drop — focus loss, `reset`, the unseeded first draw, a
    /// declined hidden relocation — is not counted).
    pub forgotten: u64,
    /// The live pool: unpaid cells in flight at the last tick.
    pub credits: usize,
    /// Presses the host typed into a tty that will never echo them —
    /// canonical no-echo, `read -s` / `sudo` / an `ssh` passphrase /
    /// `passwd` — and therefore banked NOTHING for
    /// ([`CursorGlow::note_typed_swallowed_no_echo`]).
    pub swallowed_no_echo: u64,
    /// Held parks whose RETURN came within the window and was judged as
    /// one `origin -> target` echo ([`HeldPark`]).
    pub park_returns: u64,
    /// Held parks flushed through the ordinary body at their own clock
    /// (no return: silence, an edge, or a move that was not the return).
    pub park_flushed: u64,
}

impl CursorGlow {
    /// HOST KEY-HINT: one Backspace keypress. Escalates the quench meter,
    /// cools the standing heat AND the coal bed (deleting un-earns momentum),
    /// and arms the quench classifier that LICENSES the following retreat and
    /// classifies it as a deletion. Plain arrow-left navigation never
    /// quenches.
    pub fn note_backspace(&mut self, now: Instant) {
        self.note_backspace_erasing(now, None);
    }

    /// [`Self::note_backspace`] with the erased glyph's CELL WIDTH, priced
    /// by the host from the text it committed — the only party that knows
    /// it. The in-flight law retires the press a Backspace erases, and a
    /// press is one KEY: a plain glyph banks one press for one glyph, but a
    /// committed IME run banks ONE press for every glyph in it, priced at
    /// their summed width ([`Self::note_typed_cells`]). A Backspace erases
    /// one glyph of that run, not the run, so it pays back that glyph's
    /// cells and leaves the rest in flight: a three-glyph CJK commit into
    /// a stall, one Backspace, and the surviving two glyphs' +4 echo is
    /// still the pool's to pay (retiring the whole slot refused it
    /// `no-fresh-hint`, four dark cells for glyphs the hand never erased).
    /// `Some(0)` is a PRICED zero — a zero-width cluster the run carried
    /// (a base-less combining mark, a stray selector) advanced no cell
    /// and pays none back, and every press stands. `None` — an erase the
    /// host could not price (no typed dispatch to remember, a run already
    /// erased through) — is the whole-slot retire the plain wrapper always
    /// was: one key, one glyph, however wide. The two must not share a
    /// value: read as "unpriced", a zero-width glyph's erase retired the
    /// press BEFORE the run for a glyph it never laid.
    pub fn note_backspace_erasing(&mut self, now: Instant, cells: Option<u16>) {
        self.recent_typed_run = None;
        // Backspace establishes a new deletion class. Close every older
        // movement class and, critically, the swallowed typed cohort's
        // unspent admission credits before arming quench/poof state below.
        // `clear_typed` first flushes a held park, so its `Move` precedes
        // the `Erase` in v2's event order ([`HeldPark`]).
        self.clear_typed(now);
        // THE KEY IT ERASES (the in-flight law): a Backspace typed into a
        // stall erases the newest press still in flight — the
        // app processes bytes in order — so that one press is retired, and
        // the rest keep waiting for the row (`abc⌫d` into a stall echoes as
        // three cells against `a`, `b` and `d`). Not a forget: the row is
        // still the hand's. A press that banked several glyphs (an IME
        // commit) gives back only the erased glyph's cells when the host
        // could price them — none for a zero-width cluster, which is a
        // price, not the absence of one.
        match cells {
            Some(cells) => self
                .type_press_ring
                .retire_newest_cells(u8::try_from(cells).unwrap_or(u8::MAX)),
            None => self.type_press_ring.retire_newest(),
        }
        // The two poof licenses are one pending authority, not an event queue.
        // A newer ordinary Backspace must not borrow an unanswered kill's
        // clause-scale fallback or swoosh.
        self.kill_hint = None;
        self.unsettle();
        self.quench = (self.quench + Self::QUENCH_GAIN).min(1.0);
        self.heat *= Self::QUENCH_COOL;
        self.coal *= Self::QUENCH_COOL;
        // The canonical metric: deletes NEVER build — they mildly drain (and
        // spend the pending gap credit), per the one-metric law
        // ([`crate::typing_momentum`]). Stamped at the KEY, not the echo, so
        // an echo the shell swallows (start-of-line) still un-earns.
        self.momentum.delete(now);
        // …and the MIRROR metric builds, on the same law, from the same edge:
        // one erase is one advance of [`Self::erase_mom`]. Momentum going
        // forward and momentum going backward are now measured identically.
        self.erase_mom.advance(now);
        self.quench_hint = Some(now);
        // Schedule the plain-Backspace poof: `poof_scan` ORs this into the
        // `fresh_kill` gate, and the shared FULL-trust span/fallback machinery
        // covers the erase (a ContentOnly probe licenses nothing — see
        // [`ProbeTrust`]).
        self.bs_poof_hint = Some(now);
        // Stamp the PRE-erase row fill while it is still the pre-erase row (see
        // [`Self::bs_baseline`]).
        self.bs_baseline = self.row_fill_at_key();
        // SEAM POINT 2 (§17.2, D14): one Backspace → v2's `Erase`, on the
        // key's own edge, BESIDE the v1 bookkeeping above (the classifier
        // still reads every stamp it just wrote).
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::Erase, now);
        }
    }

    /// HOST KEY-HINT: a NAVIGATION keypress (Ctrl-A/Ctrl-E, Home/End, arrow
    /// keys, word/line motions). It LICENSES the move that follows it (a real
    /// key was pressed) and classifies that move as SCRUBBING, which earns no
    /// heat and fires no meteor. Unlike [`Self::note_backspace`] the hint never
    /// touches heat/coal, so a live blaze from real typing keeps cooling
    /// naturally while you navigate.
    pub fn note_navigation(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.flush_held_park();
        if self.newline_hint.take().is_some() && self.v2.engaged() {
            self.v2.on_event(rk::Event::CancelComposerNewline, now);
        }
        self.unsettle();
        // A newer, stronger input class supersedes a swallowed Tab/paste. If
        // its own echo is swallowed too, the weaker hint must not survive to
        // license an unrelated program move later in the freshness window.
        self.user_gesture_hint = None;
        self.nav_hint = Some(now);
    }

    /// HOST KEY-HINT: one PLAIN typed-glyph keypress (Character/Space echoes
    /// only — the host must NOT arm this for Enter, Tab, navigation keys, or
    /// modified chords). It LICENSES the move that follows it and classifies a
    /// paired one-row cursor move whose delta exceeds the typed advance as a
    /// TYPED RE-ANCHOR — a TUI input box (Claude Code's Ink prompt) rewraps its
    /// whole inset box per keystroke, so the wrap move lands left of its launch
    /// column WITHOUT the caret ever travelling through the interpolated cells
    /// (see the wrap classifier in `classify_move`).
    pub fn note_typed(&mut self, now: Instant) {
        self.note_typed_cells(now, 1);
    }

    /// [`Self::note_typed`] with the committed text's CELL WIDTH: the host
    /// prices a wide glyph at 2 and an IME commit at its summed cells, so the
    /// coalesce press budget stops structurally refusing CJK typing (see the
    /// [`Self::type_press_ring`] doc). Clamped to the sweep max — a
    /// single event can never bank more credit than one observed move can
    /// spend. Everything a plain keyboard produces goes through the 1-cell
    /// [`Self::note_typed`] wrapper unchanged.
    pub fn note_typed_cells(&mut self, now: Instant, cells: u16) {
        self.note_typed_glyph(now, cells, false, rk::TypedClass::Glyph);
    }

    /// [`Self::note_typed_cells`] with the GLYPH spelled by the host — what
    /// Rainbow Kitty v2's hero deal needs (§5.8: a shifted capital or `!`
    /// earns an m1; a space lays ribbon and deals no star) and an echo cannot
    /// tell, so the host prices it at the key exactly as it prices the
    /// click's shiftedness ([`Self::cue_keystroke_shifted`]). The v1 body is
    /// the plain wrapper's, unchanged; only SEAM POINT 2 reads the extra
    /// fields, and a host that keeps calling the wrapper is byte-identical.
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
        // SEAM POINT 2 (§17.2, D14): the typed key → v2's `Typed`, the ONLY
        // event that builds its spine, beside the v1 stamps the classifier
        // and the press-credit ring still read. The width is the SAME bound
        // the credit took: a saturated commit (`committed_text_cells` caps at
        // `u16::MAX`) would lay a ribbon folding up through the whole grid
        // at O(n²) — measured 17 701 cells, a 96 ms tick on a 300×60 grid —
        // for a key whose echo v1 would refuse past the cap anyway.
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

    /// Record a main-screen ENTER classifier stamp. It LICENSES the move that
    /// follows it, and a FRESH stamp is the bare-Enter license for the jump
    /// arm ("a cold Enter throws the streak again").
    pub fn note_return(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.flush_held_park();
        self.unsettle();
        self.user_gesture_hint = None;
        self.return_hint = Some(now);
        // SEAM POINT 2 (§17.2, D14): the Enter KEY → v2's `Return` — inert
        // in every producer by design (T1: a keypress is not a move); the
        // flight arrives as the observed move seam point 1 hands over with
        // `Licence::Return`.
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::Return, now);
        }
    }

    /// Record a Tab / scripted-gesture classifier boundary. A human touched
    /// the keyboard, so it LICENSES the move that follows — bounded, like
    /// every other term, by [`Self::USER_GESTURE_HINT_FRESH`]. A paste does
    /// not ride this class: its stamp is revoked at the boundary (enqueue
    /// is not delivery) and the DELIVERED insert arms
    /// [`Self::note_insert_delivered`] instead.
    ///
    /// THE SUPERSEDE SHAPE, NOT THE WIPE: a gesture goes through
    /// [`Self::supersede_typed_press`] and KEEPS the banked typed stamps —
    /// each a real key whose echo is still in flight, so an in-flight glyph
    /// echo stays licensed under its own class while the Tab's sweep is
    /// licensed by the gesture hint (wiping the bank for a mid-burst Tab
    /// manufactured the exact 8-cell black notch the bank was built to
    /// remove) — and the Return, nav, quench and composer-newline one-shots
    /// exactly as a glyph does: an arrow's hop or a Backspace's retreat
    /// still in flight when the Tab is pressed is that key's echo, not the
    /// gesture's landing, and the gesture is left for the completion that
    /// follows it. The classic trail's gesture still drops its nav hint
    /// (its Tier-1 binding of a gesture press disposing of the live
    /// classes stands), a divergence confined to an arrow whose hop is
    /// still in flight when the Tab is pressed.
    pub fn note_user_gesture(&mut self, now: Instant) {
        self.flush_held_park();
        self.supersede_typed_press();
        self.unsettle();
        self.user_gesture_hint = Some(now);
    }

    /// HOST DELIVERY EDGE, the KEY classes: a key queued behind a draining
    /// paste banked its licence at dispatch and had it revoked when its
    /// write did not happen inline (`revoke_input_hints_at`); the writer
    /// thread's completed write is when its echo can begin, so the licence
    /// is re-stamped at that instant — one stamp, one echo, exactly as the
    /// inline key's. A PURE re-stamp: no supersede, no second v2 event —
    /// the credit, the v2 `Typed` / `Return` / `Erase` / `Kill`, the credit
    /// retire and forget, the quench thermals and the erase momentum went
    /// out at the key and were never revoked, and `note_motion`'s
    /// `clear_typed` would wipe the typed stamps of glyphs delivered in the
    /// same batch. The erase classes read the pre-erase row fill from the
    /// PREVIOUS probe first ([`Self::row_fill_at_delivery`]). The delivered
    /// insert is its own edge ([`Self::note_insert_delivered_from`]).
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

    /// The row fill an erase KEY stamps as its pre-erase baseline
    /// ([`Self::bs_baseline`]): this frame's probe if one has landed,
    /// otherwise the last presented truth.
    pub(super) fn row_fill_at_key(&self) -> Option<(u16, u16)> {
        self.row_cur_meta
            .or(self.row_prev_meta)
            .map(|m| (m.row, m.fill))
    }

    /// The row fill an erase DELIVERY stamps as its pre-erase baseline: the
    /// PREVIOUS probe first — the prelude applies receipts after this
    /// frame's `observe_row_with_trust`, so `row_cur_meta` may already be
    /// the post-erase row.
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

    /// HOST DELIVERY EDGE: an insert's bytes — a file drop, ⌘V, the `paste`
    /// verb, a Tab completion, a raw ⌃V — have PROVABLY landed on the wire
    /// (the FIFO writer's completed write; the dispatch instant for a
    /// synchronous insert), `width` as the host priced them from the text
    /// it put on the wire ([`InsertWidth`]). Arms the [`InsertLicence`]
    /// with the ROW WITNESS read now ([`Self::hand_row`]): the ONE same-row
    /// forward echo that fits it is laid as a single [`rk::Event::Sweep`]
    /// joining the live cohort, so the walk continues through the inserted
    /// span with no seam (see the insert arm in [`Self::spawn`] and the
    /// anchored lane's twin in [`Self::echo_anchor_pass`]).
    ///
    /// A PURE STAMP, on purpose: it supersedes nothing (a Return or an arrow
    /// pressed between the delivery and the next frame keeps its licence in
    /// every style), it survives the class-changing supersedes (a ⌃V's own
    /// modifier release, a nav press, `clear_typed`) and yields to a fresher
    /// class at the seam instead, and every read of it is Rainbow Kitty-
    /// gated, so the other styles are byte-identical by construction. A
    /// second delivery before the first is spent ACCUMULATES (a multi-file
    /// drop is one paste per file, and the shell echoes them as one hop). A
    /// hop of exactly this width the seam refused within
    /// [`Self::INSERT_RETRO_FRESH`] is retro-licensed ([`PendingHop`]).
    /// `Cells(0)` arms nothing.
    ///
    /// A receipt knows when its bytes were DISPATCHED: the late-receipt claim reaches back
    /// `INSERT_RETRO_FRESH` from the writer's completion instant, which for
    /// a Tab queued behind a short-draining paste reaches past the Tab's
    /// own press, so the claim also requires the hop to have been refused
    /// AT OR AFTER `dispatched_at` — the key's bytes could not have echoed
    /// before they were on their way (a program hop refused before the key
    /// was dispatched is not the insert). One rule for every class — a
    /// paste's bytes could only echo after its dispatch too.
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
        // THE LATE RECEIPT ([`PendingHop`]): the frame that observed the
        // echo already refused a same-row forward hop inside the retro
        // window that was this insert's, and it is laid now on the delivery
        // clock — a priced width by exactly its width; the unknown class by
        // the hop on the row the hand was on, no wider than its bound (the
        // multi-line paste whose receipt a preempted writer thread
        // publishes after the frame that saw `[Pasted text #1 +N lines] `
        // echo, the Tab queued behind a draining paste). The anchored lane
        // branded the row for that keyless-looking advance (a never-typed
        // row has no established identity to exempt it); the brand was this
        // insert's echo and is lifted with it.
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

    /// THE DISPATCH-TIME ARM: a bare Tab / ⌃V whose write is inline arms
    /// the insert licence at dispatch — the [`Self::note_insert_delivered`]
    /// arm WITHOUT the late-receipt claim. The retro window is symmetric in
    /// time (it exists for a spill-held receipt resolved at the reader's
    /// clock), so at dispatch it would admit a hop refused up to 0.25 s
    /// BEFORE the press — a prompt print, a spinner tick on the hand's row
    /// — as the Tab's echo while nothing of the key was on the wire yet. A
    /// still-fresh pending hop is left in place for a real receipt to
    /// claim. The licence is stamped at `at`, so a queued Tab's revoke at
    /// its instant finds it, undoes the tally, and the delivery re-arm
    /// counts the gesture once and performs the (legitimate) retro claim.
    pub fn note_insert_armed(&mut self, at: Instant, width: InsertWidth) {
        let _ = self.arm_insert(at, at, width);
    }

    /// The arm shared by [`Self::note_insert_delivered`] and
    /// [`Self::note_insert_armed`]: the width resolved, the ROW WITNESS
    /// read NOW — before the echo moves anything: the host arms a Tab / ⌃V
    /// at dispatch and a paste at the frame that first observes its echo,
    /// ahead of that frame's anchor feed and tick, so what the engine
    /// remembers is where the hand was — and the seam armed
    /// ([`InsertSeam::arm`]). Returns the licence, or `None` for `Cells(0)`.
    pub(super) fn arm_insert(
        &mut self,
        at: Instant,
        dispatched_at: Instant,
        width: InsertWidth,
    ) -> Option<InsertLicence> {
        let (cells, known) = match width {
            InsertWidth::Cells(0) => return None,
            InsertWidth::Cells(cells) => (cells, true),
            InsertWidth::Unknown => (Self::INSERT_GESTURE_CELLS, false),
        };
        self.recent_typed_run = None;
        self.unsettle();
        let row = self.hand_row(at);
        Some(self.insert.arm(at, dispatched_at, cells, known, row))
    }

    /// THE HAND'S ROW at `now`, read once when an insert is armed as its
    /// [`InsertLicence::row`]: the VISIBLE caret's row when there is one
    /// and it sits with the hand (the DEC cursor is where the hand is — it
    /// wins over the licensed-move memory, because a prompt the program
    /// printed after a command's output moved the caret there without a
    /// licensed move; it yields only to the anchored lane's own testimony,
    /// below); under a hidden caret, the row of the last licensed move (a
    /// typed or anchored echo, an insert, a rewrite) within the ribbon's
    /// chain window, else the ESTABLISHED echo row
    /// ([`Self::last_anchor_sweep`] — identity, not freshness: the repro's
    /// drop lands after a long quiet gap); `None` when nothing can say (a
    /// hidden caret on a never-typed row).
    ///
    /// THE PARKED CARET: a visible DEC cursor on a row OTHER than the one
    /// the anchored lane established as the echo row (`fc_parked.py`'s CUP
    /// 1;1 before show: the input row echoes through
    /// [`Self::echo_anchor_pass`] while the caret sits at the origin) is
    /// NOT where the hand is — taking the parked row as the witness let a
    /// Tab or ⌃V spend on the status row the moment the TUI walked that
    /// row's cursor forward ≤ 32 cells, and refused the completion on the
    /// input row as not its own. Under that shape the witness is the
    /// lane's own, exactly as under a hidden caret: the last licensed move
    /// within the chain window, else the established echo row — never the
    /// print anchor, which a status repaint or a transcript line may hold.
    /// The sweep's ROW is the proof, and it is current: parked only when
    /// the established echo row is not the caret's. A visible caret that
    /// merely differs from the print anchor is Codex's — the hand IS on the
    /// caret's row while the transcript prints above it — and a caret on
    /// the established row is the hand's by the lane's own testimony;
    /// neither is ever demoted. The residual is the hidden branch's own: a
    /// caret shown on a row the hand reached without a scroll or band move
    /// since the lane last licensed elsewhere (a prompt on a fresh row
    /// after a short command's output) reads as parked until a typed echo
    /// or the next licensed anchored echo names its row.
    pub(super) fn hand_row(&self, now: Instant) -> Option<u16> {
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

    /// Remember a refused same-row forward hop `col0 → col1` on `row` for
    /// ONE delivery receipt still in flight ([`PendingHop`]) — Rainbow Kitty
    /// only, the one style with a delivered-insert class to claim it; every
    /// other style records nothing. The one memory both the visible-caret
    /// gate (`spawn_judged`) and the anchored lane (`echo_anchor_pass`) keep.
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

    /// Move the insert's ROW-ADDRESSED members with a scroll or a band move
    /// under `map` ([`InsertSeam::translate_rows`]); the lane's own row
    /// witness (the last licensed move's row) names an absolute row too and
    /// is dropped with them. The one law both family translations enforce.
    pub(super) fn translate_insert_rows(&mut self, map: &impl Fn(u16) -> Option<u16>) {
        self.insert.translate_rows(map);
        self.last_licensed_row = None;
    }

    /// How many cells the fresh delivered insert `ins` may buy on a same-row
    /// forward echo: its own width plus the unpaid typed credits queued
    /// behind it (a key typed behind a draining paste echoes in the same
    /// hop).
    pub(super) fn insert_reach(&self, ins: InsertLicence, now: Instant) -> usize {
        usize::from(ins.cells) + self.typed_credits_within(now)
    }

    /// THE INSERT ECHO SHAPE: a same-row FORWARD move no wider than the fresh
    /// insert's reach, under Rainbow Kitty. `None` for every other shape,
    /// style or a stale/absent stamp — the move is then program output to
    /// this class exactly as before.
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

    /// ROW IDENTITY for the insert: the stamp is delivery CORRELATION, not
    /// row identity — for two seconds after a drop a spinner row, a token
    /// counter or an elapsed timer advances inside its window too. A row may
    /// spend the insert when it is the ROW THE HAND WAS ON when the insert
    /// was armed ([`InsertLicence::row`], read at the arm from the last
    /// licensed move or the visible caret), or — a PRICED width only — when
    /// the advance is the insert's WHOLE width (a spinner crosses one digit;
    /// a 63-cell path echoes as 63 cells, on a hidden-caret row nobody has
    /// typed on yet). An insert of UNKNOWN width (a Tab completion, a raw
    /// ⌃V, a multi-line body — bounded at [`Self::INSERT_GESTURE_CELLS`])
    /// has no width to be recognised by and is spent on the witnessed row
    /// alone: NOT the prompt a later Return printed on the next row (the
    /// first cut's `established` term let that through — Return's landing
    /// establishes the new row), not the mode line a ⇧Tab repaints, not any
    /// row nobody typed on. What is left is refused as a program row.
    pub(super) fn insert_row_identity(row: u16, hop: usize, ins: InsertLicence) -> bool {
        let own_row = ins.row == Some(row);
        own_row || (ins.known && hop >= usize::from(ins.cells))
    }

    /// THE INSERT'S ECHO ON THE VISIBLE LANE — the one predicate
    /// `spawn_judged`'s insert arm lays by on [`SpawnLane::Visible`] and the
    /// hide bridge admits a hidden→visible reappearance by
    /// ([`Self::hidden_bridge_source`], 2026-09-22), so the two can never
    /// disagree about one move: the echo shape ([`Self::insert_echo`]), on
    /// the insert's own row ([`Self::insert_row_identity`]), with no fresher
    /// press class owning the hop ([`Self::fresher_class_owns_hop`]).
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

    /// Whether a FRESHER press class owns this same-row forward hop, so the
    /// insert must wait for its own echo: a navigation press (an arrow's own
    /// hop), a reflow, a scripted gesture stamped at another instant, or
    /// unpaid typed presses dispatched before this insert whose credits
    /// cover the whole hop (the per-key path and a coalesced batch). Keys
    /// dispatched behind the insert cannot have echoed ahead of its bytes
    /// on the FIFO wire, even when a delayed frame finds all their credits
    /// banked. Their own later echo must keep those credits.
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

    /// Whether a keyed ERASE class is fresh — a Backspace, a kill chord, a
    /// navigation press: a caret retreat under one of these is that key's own
    /// (the Erase / Kill events went out at the key, the nav has its own
    /// choreography) and is never read as a program rewrite.
    pub(super) fn erase_key_fresh(&self, now: Instant) -> bool {
        hint_fresh(self.quench_hint, now, Self::QUENCH_HINT_FRESH)
            || hint_fresh(self.kill_hint, now, Self::KILL_HINT_FRESH)
            || hint_fresh(self.bs_poof_hint, now, Self::KILL_HINT_FRESH)
            || hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
    }

    /// Whether the row probe, WHEN it speaks for `row` this frame, shows
    /// every cell at and right of `col` blank — the content proof the
    /// rewrite arm asks for: the owner's own criterion is "lit cells under
    /// blanks to the right of the caret". A frame that catches a readline
    /// whole-line redraw between its CR and the re-print observes the same
    /// backward hop with the text still on the row, and must retract
    /// nothing. A host that probes another row, or none (a direct-drive
    /// test), proves nothing either way and the geometry alone decides.
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

    /// THE INSERT'S REWRITE SHAPE: a keyless same-row RETREAT landing inside
    /// the span a delivered insert laid within [`Self::INSERT_REWRITE_FRESH`]
    /// — Claude Code swapping the dropped path for `[Image #1] ` — with the
    /// row probe (when it speaks) showing the suffix blank. Returns the
    /// cells the caret retreated by. A retreat landing LEFT of the span, a
    /// keyed retreat, a stale span or another style: `None`, and the move
    /// is judged exactly as before.
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

    /// LAY A DELIVERED INSERT'S ECHO — the licensed insert hop `pc → cc` on
    /// `row`: the one-shot is taken, the surplus beyond the insert's width
    /// (a key typed behind the paste, echoed in the same hop) is paid from
    /// the press ring and its stamps consumed, and v2 is handed ONE
    /// [`rk::Event::Sweep`] over the span on the delivery clock behind an
    /// inert typed move — the coalesced-echo shape, so the cells join the
    /// live cohort and the walk continues without a seam. NOTHING ELSE: no
    /// `classify_move` (no typed stamp popped for it), no thermals, no
    /// momentum pulse, no click, no v1 geometry — an insert is the user's
    /// gesture but it is not typing. Logged `licensed licence=insert`.
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
            // The presses the surplus drains to zero take their stamps with
            // them, so a key whose glyph echoed inside the insert's hop cannot
            // license a later program advance on its surviving stamp.
            let credits = self.typed_credits_within(now);
            let presses = self
                .type_press_ring
                .spend_counting(now, surplus.min(credits));
            for _ in 0..presses {
                let _ = self.type_hint.take_fresh(now, Self::TYPE_HINT_FRESH);
            }
        }
        // An unknown width pays no surplus below its bound; the presses
        // banked at or before its delivery are bounded away later instead
        // ([`Self::insert_orphans`]).
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
        // A Tab / ⌃V arms the gesture class at the same instant for the
        // classic trail's lockstep and for a cross-row completion; once the
        // insert took its same-row hop that one-shot is spent with it.
        if self.user_gesture_hint == Some(ins.at) {
            self.user_gesture_hint = None;
        }
        self.last_licensed_row = Some((cr, now));
        if self.v2.engaged() {
            // Born on the delivery clock (T2: the echoing frame shows the
            // run lit, no edge-in), floored like a key's sweep.
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

    /// RETRACT A DELIVERED INSERT'S SPAN to the program's new caret: v2 is
    /// handed [`rk::Event::Rewrite`] (the ribbon drains the suffix from
    /// `col` farthest-first at `12·n + 240` like a kill, the caret mirror
    /// moves, nothing is born, nothing winces, nothing sounds), the span
    /// memory shrinks to the caret, and the ring records the retreat
    /// `licensed licence=rewrite` from the caret's old column — `col +
    /// cells` on the same row, which is where [`Self::insert_rewrite`]
    /// measured the retreat from.
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

    /// `trail status`'s in-flight rows — see [`InFlightTally`]. The live
    /// pool is read at the last tick's clock (`heat_at`), so a reader gets
    /// the count the last frame judged by.
    #[must_use]
    pub fn in_flight_tally(&self) -> InFlightTally {
        InFlightTally {
            credits: self.heat_at.map_or(0, |now| self.typed_credits_within(now)),
            ..self.in_flight_tally
        }
    }

    /// A COMPOSER NEWLINE landed — an alt-screen Shift+Enter, which agent
    /// composers bind to "insert a line break without submitting" (see
    /// [`Self::newline_hint`]).
    ///
    /// The host arms this on the SAME arm that arms the typed hint for that
    /// chord, and only there: a main-screen Shift+Enter is Enter morphology, a
    /// plain Enter takes [`Self::note_return`], and a modified chord
    /// that is not a composer newline arms nothing. It never gates bytes or
    /// classifies as typing on its own. The classifier uses it to license the
    /// authored row change; v2 also receives the key-time `ComposerNewline`
    /// event, so its fresh-line walk is dated at the chord rather than the
    /// later repaint. The ribbon waits for the authored home move before a
    /// new-line glyph may spend that gate.
    pub fn note_newline_break(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.unsettle();
        self.newline_hint = Some(now);
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::ComposerNewline, now);
        }
    }

    /// Record a grid-geometry classifier boundary. Reflow itself is dark: the
    /// host resets this engine because an old cell-space path is not truthful
    /// after a resize/font/scale coordinate change.
    pub fn note_reflow(&mut self, now: Instant) {
        self.recent_typed_run = None;
        // Reflow is a newer, disjoint movement class. Close any swallowed
        // typed cohort before arming it so a resize cannot leave old credits
        // available to pool with a later key.
        self.clear_typed(now);
        self.unsettle();
        self.reflow_hint = Some(now);
    }

    /// A USER MOTION press: the host saw a key whose whole purpose is to move
    /// the cursor (the navigation key class, scroll chords like Ctrl-D). It
    /// stamps the same `nav_hint` [`Self::note_navigation`] does — one key
    /// press, one license — and the classifier reads the landing's SHAPE to
    /// decide whether the jump choreography fires.
    pub fn note_motion(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.clear_typed(now);
        self.unsettle();
        self.nav_hint = Some(now);
        // A real navigation press SUPERSEDES a kill's unpaid retreat: the move
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

    /// How many committed CELL CREDITS are in flight — unpaid and inside
    /// [`IN_FLIGHT_PATIENCE_S`] — the press BUDGET consumed by the
    /// anti-stray gates (see [`Self::type_press_ring`]). Capacity-bounded by
    /// the ring; reads only.
    pub(super) fn typed_credits_within(&self, now: Instant) -> usize {
        self.type_press_ring.cells_within(now)
    }

    /// **THE SHARE RULE**: whether `paid` cells from `credits` unpaid presses
    /// buy a `cells`-wide typed hop — at least two presses, and at least
    /// three quarters of the swept cells. MOSTLY PAID FOR, NOT EXACTLY PAID
    /// FOR: requiring `credits >= cells` exactly tore holes in the ribbon —
    /// a real burst whose ring had just dropped one press was refused
    /// ENTIRELY, a gap where the user typed. The anti-stray shape the budget
    /// exists to refuse is ONE press echoing as a MULTI-cell hop, funded at
    /// 1/N, which is a different shape from a burst one credit short, funded
    /// at (N-1)/N. `w` over a five-cell word is 1 credit against 5: refused
    /// by both clauses. Eight typed cells backed by seven presses: 28 >= 24,
    /// admitted, and it paints instead of tearing. THE FLOOR IS DELIBERATELY
    /// RAW: `credits >= 2` reads the ledger, never an uprate — one press
    /// whose echo has not come is indistinguishable from a stale stamp, and
    /// one credit is one credit however wide the cells beneath it are.
    pub(super) fn share_rule(credits: usize, paid: usize, cells: usize) -> bool {
        credits >= 2 && paid * 4 >= cells * 3
    }

    /// Whether the unpaid presses pay for the same-row forward hop `pc → cc`
    /// on `row` under the share rule ([`Self::share_rule`]).
    ///
    /// **A PRESS PAYS FOR THE CELLS ITS OWN GLYPH OCCUPIES.** The host prices
    /// the width it can SEE — a committed IME run, a wide character it put
    /// on the wire. What it cannot see is a PROGRAM that echoes a different
    /// glyph from the key (a terminal-side input method, a TUI rendering
    /// full-width forms, a remote editor): there the host banks ONE cell per
    /// press while the caret advances TWO, and the share reads `4N >= 6N` —
    /// false for every N — so a wide batch was only ever funded by credits
    /// left over from a batch ALREADY REFUSED, and the measured ribbon
    /// painted in alternating blocks (fifteen-cell black runs at 10 keys/s
    /// into a 700 ms repaint). So the run's own cells are asked how wide
    /// they are, from the grid: [`Self::swept_wide_continuations`] counts
    /// the `'\0'` continuation columns the row probe carries, captured under
    /// the same terminal lock as the move. Each unpaid press may claim at
    /// most ONE of them — one press lays one glyph, and a grid glyph is at
    /// most two cells wide, so a press can be owed at most one column more
    /// than the host priced it at — which is why the uprate is bounded by
    /// the PRESS count: over an all-wide run the rule reduces to
    /// `presses >= 0.75 * glyphs`, the narrow rule restated in glyph space,
    /// not a loosening of it.
    pub(super) fn typed_share_paid(&self, row: u16, pc: u16, cc: u16, now: Instant) -> bool {
        let credits = self.typed_credits_within(now);
        let paid = credits.saturating_add(
            self.swept_wide_continuations(row, pc, cc, now)
                .min(self.type_press_ring.presses_within(now)),
        );
        Self::share_rule(credits, paid, usize::from(cc.saturating_sub(pc)))
    }

    /// How many columns of the same-row run `[from_col, to_col)` are WIDE
    /// CONTINUATIONS — the second cell of a CJK / emoji glyph.
    ///
    /// **The width is the grid's, never a guess.** The host's per-frame row
    /// probe ([`Self::observe_row`]) is captured under the same terminal lock
    /// as the move being classified and spells a wide glyph's continuation
    /// column `'\0'` (`Terminal::row_cols_into`'s convention, the one the poof
    /// detector's column math already depends on). A zero-width joiner sequence
    /// and a `COMPLEX` cluster reach the probe as their RESOLVED lead char plus
    /// that same `'\0'`, so they count once and pay for two cells exactly like
    /// any other wide glyph; a combining mark advances the caret by no column at
    /// all and so appears in no swept run.
    ///
    /// `0` — the answer that changes nothing — whenever the probe cannot speak
    /// for this row: no probe at all (a headless host, a direct-drive test, a
    /// scrolled-back or unwired frame), a probe of a DIFFERENT row, or one older
    /// than [`Self::POOF_PROBE_STALE`]. Both [`ProbeTrust`] classes are read:
    /// this is a pure content measurement of cells the probe already carries,
    /// which is precisely what `ContentOnly` exists to serve, and it licenses
    /// nothing on its own.
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

    /// HOST KEY-HINT WITHHELD: a plain typed glyph the host DID write to the
    /// PTY but typed into a tty in CANONICAL NO-ECHO mode (`read -s`, `sudo`,
    /// an `ssh` passphrase, `passwd` — iTerm2's password-mode rule, read off
    /// the master's termios at the key), so the press will never be echoed
    /// and there is no cell for its credit to pay for. The host calls THIS
    /// instead of [`Self::note_typed_glyph`] for that press: no typed stamp,
    /// no press credit, no v2 `Typed`, no supersede of the other classes —
    /// nothing a same-row program write inside the ten-second patience could
    /// spend as a phantom (the `read -s` half of the same-row swallowed-press
    /// residual). The only trace is the `swallowed_no_echo=`
    /// tally on `trail status`, so a dark password prompt reads as a
    /// verdict and not a mystery. A raw-mode program (ECHO clear, ICANON
    /// clear — Claude Code, vim, readline at rest, and bash's `read -s -n`,
    /// which is raw by termios) echoes for itself and never comes here.
    pub fn note_typed_swallowed_no_echo(&mut self) {
        self.in_flight_tally.swallowed_no_echo += 1;
    }

    /// FORGET every in-flight press — an observed edge the presses cannot
    /// explain (the in-flight law): a keyless hop the echo shape
    /// refuses (backward, cross-row), a forward hop the share rule refuses
    /// (`no-credits`) or the cap refuses, a row change no key licensed, a
    /// non-typed licence (Return, an arrow, a scripted gesture — the row the
    /// presses were on is closed), a kill at the key. The mirror of the
    /// erase/kill/focus clears the engine's echo ledger already makes
    /// (`Engine::echo_bridge`) — NOT of its scroll and band clears
    /// (`Engine::translate_scroll` / `translate_band`): a scroll-translated
    /// echo keeps its host pool (see the forget edges in `spawn`), and the
    /// ledger alone loses its hole-bridging there. So a swallowed press — a
    /// password, a pager's `q`, a modal's keys — cannot roll forward as a
    /// phantom credit for the whole [`IN_FLIGHT_PATIENCE_S`]: it lives only
    /// until the next edge, which every dismissal is. Counted for `trail
    /// status` (`inflight_forgotten=`). A refused same-row FORWARD hop deliberately
    /// does not forget: with no stamp fresh it means the pool is empty, or
    /// holds one credit and the hop was wider than one cell (a one-press
    /// multi-cell hop — vim `w` on a stale stamp, or a wide echo the host
    /// priced at one cell); that credit is KEPT for the delivered insert's
    /// receipt ([`PendingHop`] / `insert_reach`) and the next key's ledger
    /// bridge. A one-press +1 is licensed, not refused (`unpaid_typed_echo`).
    ///
    /// COUNTED ONLY FOR A LIVE PRESS: a press past the patience is retired
    /// physically only at a hidden→visible boundary
    /// (`retire_hidden_movement_provenance`), so on a visible caret a dead
    /// slot can sit in the ring until the next edge. Every licence read is
    /// life-filtered and already reports that pool as empty (`credits=0`),
    /// so an edge that clears nothing but dead slots is not a forget — the
    /// slots are still blanked, but `inflight_forgotten=` rises only when a
    /// press the clock had not retired went with them.
    pub(super) fn forget_typed_credits(&mut self, now: Instant) {
        self.insert.orphans = None;
        self.insert.orphan_exact = None;
        if self.type_press_ring.is_empty() {
            return;
        }
        let live = self.type_press_ring.presses_within(now) > 0;
        self.type_press_ring.forget();
        if live {
            self.in_flight_tally.forgotten += 1;
        }
    }

    /// Whether this move is a SAME-ROW FORWARD ECHO with an unpaid press behind
    /// it — the licence of last resort for a repaint that landed after every
    /// banked stamp went stale (see the seam in [`Self::spawn`] and the
    /// classifier fallback in `classify_move`). Rainbow Kitty only: its ribbon
    /// is a per-cell record of the keys, so a key with no cell is a defect there
    /// in a way it is not for a comet; every other style keeps the stamp window
    /// byte-for-byte.
    ///
    /// THE ONE-PRESS ECHO (the stalled last key): a SINGLE
    /// unpaid press licenses exactly ITS OWN +1 on the row it was pressed on.
    /// The stamp was never the licence; the press in flight is, and one press
    /// whose glyph has not been laid describes a one-cell advance completely.
    /// Every wider hop keeps the two-press floor and the share rule
    /// (`rainbow_coalesce`), so vim's `w` on a stale stamp is refused as
    /// before. The stray this admits is bounded to one cell at the caret's
    /// own cell, once: the credit is spent on admission.
    ///
    /// THE FOLD (the stalled key whose echo wraps the row):
    /// the tight wrap shape ([`Self::fold_shape`] — down one row from the
    /// row's last two columns to the next row's first two) is the echo
    /// shape too, paid EXACTLY: one press per cell the fold lays (1..=3),
    /// stricter than the share rule at that width. The COALESCED fold
    /// (`shape_wrap`'s second arm, [`Self::coalesced_fold_shape`]: a stalled
    /// batch whose drain crosses the wrap wider than the tight fold, landing
    /// at the new row's columns 1..=3) is admitted on the pool too, with
    /// EXACT payment — `credits == cells`: the arm's
    /// own witness is a live typing rhythm a stall ages out, so the pool
    /// must describe the hop completely, and a batch that overpays it is
    /// refused (and, as every cross-row refusal, forgotten). Refused and
    /// forgotten, every cell of the drain stayed dark on both rows.
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
            // EXACT for one cell, AT LEAST TWO for anything wider — the floor
            // the coalesce's own share rule carries. One press whose echo has
            // not come funds one cell and nothing more; two or more is a
            // BATCH still in flight, judged under the share rule.
            return credits >= usize::from(cc - pc).min(2);
        }
        if let Some(cells) = self.fold_shape(pr, pc, cr, cc, geom) {
            return cells >= 1 && credits >= cells;
        }
        self.coalesced_fold_shape(pr, pc, cr, cc, now, geom)
            .is_some_and(|cells| credits == cells)
    }

    /// The ALT-SCREEN blink discriminator: the host's repaint blink
    /// ([`Self::note_repaint_blink`] — a DECTCEM hide inside a DEC 2026
    /// synchronized update, the per-keystroke full-redraw bracket) is
    /// younger than [`Self::BLINK_HINT_FRESH`]. On the alt screen it
    /// separates a full-redraw TUI's re-anchor and coalesce from a
    /// deliberate vim/less jump; the main screen never asks.
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

    /// A move's PANE-LOCAL columns, for the fold shapes: `(pane_cols,
    /// pc_local, cc_local, pane_contains_move)` under the host's pane
    /// columns ([`Self::pane_columns`]) or the whole grid.
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

    /// **THE SOFT-WRAPPED CARET** (2026-09-23, the owner's Claude Code
    /// composer: a word typed INTO the text at the fold left a band stub in
    /// the continuation row's blank indent, and the word's first letter dark
    /// once the next key carried it down). Claude Code 2.1.280 draws the key
    /// that fills row `r`'s last text column THERE, and moves the caret
    /// ALONE to the continuation row's indent `(r + 1, 2)`. The next key
    /// reflows the partial word down to the indent. The coalesced fold and
    /// the typed re-anchor lay the landing row's head (`landing − 1`), where
    /// the 2.1.278 end-of-text wrap puts the wrap key's glyph; here that
    /// cell is the blank indent.
    ///
    /// Read off the probes, not the arithmetic: a typed move exactly one row
    /// down whose landing row is BLANK from the pane's first column up to the
    /// landing, while the ORIGIN cell `(pr, pc)`, blank in the last probe
    /// (taken with the caret there), now holds a glyph in the row above the
    /// caret, every cell of that row left of the origin as it was. Returns
    /// the first column of the word that glyph ends on the origin row, or
    /// `None` off the shape. A Space typed there leaves the origin blank
    /// and stays on the fold shapes' model: no take has pinned what Claude
    /// Code draws for it.
    ///
    /// **…OR OVER THE GLYPH IT PUSHED ON** (2026-09-23 review,
    /// `tests/wrap_code_reflow.rs`, B4): a key inserted at the row's last
    /// text column into a token the box HARD-breaks there (Ink's wrap-ansi
    /// `hard`: a token begun at the row's first text column cannot go down
    /// whole) is drawn at the origin over the glyph it pushed on to the
    /// next row, and the caret alone wraps to the indent before it. The
    /// origin was not blank, but it changed, to a glyph a FRESH press
    /// stamped ([`PressCredits::fresh_glyph`]). A glyph that stayed (a
    /// shell's pending wrap parks the caret ON the row's last glyph) or
    /// became one no key stamped (a host that stamps cell counts only
    /// proves nothing) is no key drawn there. Refused, that move was the
    /// coalesced fold's: its cell on the indent, the key's glyph dark, and
    /// the re-wrap relay's measure fast-retracting the whole row.
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
        // …AND NOTHING LEFT OF THE ORIGIN MOVED: the key went in AT the
        // fold, so the origin row reads as it did from the pane's first
        // column up to the origin (every soft wrap in the real 2.1.280 takes
        // and B4's hard break keep that prefix). zle's Ctrl-R switching to
        // an older multi-line entry has the same shape — its indented
        // continuation starts with the match, and its first line may hold
        // the key's glyph at the old caret column (`cat a` → `cd /d;` on
        // `d`) — but it rewrites that prefix. Admitted, the soft wrap moved
        // the landing gate's witness to the origin, where the key's glyph
        // passed it, and the hop laid its cell under history text.
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

    /// **THE SOFT-WRAPPED WORD CAME DOWN** (2026-09-23 review,
    /// `tests/wrap_code_reflow.rs`, D): whether the forward echo to
    /// `(cr, cc)` from a soft-wrapped caret's landing carried the word the
    /// wrap key ended off its row — every column of it, `sw.word_col0` to
    /// `sw.origin.1`, blank now in the probed row above the caret. The key
    /// that reflows it erases it there and rewrites it from the indent; a
    /// burst or a paste after a short word hops the caret as far and leaves
    /// the word standing. `false` wherever the probe is not this move's or
    /// the row above was not probed.
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

    /// **THE LIFTED WORD** (2026-09-23, the owner's Claude Code composer, the
    /// reverse re-wrap). A word typed at the start of the continuation row
    /// and glued to the text after it (`already[Image #1]`) splits on the
    /// Space that follows it, and the word alone fits the row above again:
    /// Ink rewrites it at that row's end and the rest of the continuation
    /// row from the indent, the caret before `[Image` — a same-row backward
    /// move to the word's old first column. The seam held it as Ink's park
    /// and the ribbon then drained it as a re-anchor: the word's light went
    /// under the chip's text and the word stood unlit on the row above.
    /// Now such a move is never parked ([`Self::park_candidate`]), lays no
    /// landing (its landing is the word's old first column, not a glyph the
    /// key wrote), and hands the ribbon the verdict with its `Move`
    /// ([`rk::ribbon::Lift`]).
    ///
    /// Read off the probes: a backward move on one row whose span
    /// `cc..pc` held ONE word (no blank) in the last probe and holds other
    /// text now, while the row above — probed both times — now ENDS with
    /// exactly that word after a blank, where it was blank before. Returns
    /// the column the word starts at on the row above, or `None` off the
    /// shape (and wherever a probe is missing).
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

    /// THE TIGHT FOLD SHAPE, stated once for the classifier (`shape_wrap`'s
    /// first arm in `classify_move`) and the in-flight seam
    /// ([`Self::unpaid_typed_echo`]) so the two cannot disagree: down
    /// exactly one row, landing at the pane's first or second column, from
    /// the pane's last or second-to-last column (a deferred-wrap echo lands
    /// at col 1; a wide glyph wraps from the second-to-last column). Returns
    /// the cells the fold LAYS — the origin row's tail from `pc` to the
    /// pane's edge plus the landing row's head before `cc`: 1 for
    /// `99 → (r+1, 0)`, 2 for `98 → (r+1, 0)` or `99 → (r+1, 1)`, 3 for
    /// both — or `None` off the shape. The tail is 0 when the fold leaves
    /// the pane's last column and that cell was already paid as the
    /// pending-wrap print ([`Self::fold_origin_tail`], [`Self::wrap_paid_row`]):
    /// `99 → (r+1, 0)` then lays 0 and `99 → (r+1, 1)` lays 1.
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

    /// The cells a fold lays on its ORIGIN row: `pc` to the pane's edge —
    /// unless the fold leaves the pane's last column and the anchored lane
    /// already licensed that cell as the pending-wrap print
    /// ([`Self::wrap_paid_row`]), in which case the fold carries only the
    /// landing row's head. A last-column glyph arriving WITH the wrap set
    /// no witness and prices both cells as before.
    pub(super) fn fold_origin_tail(&self, pr: u16, pc_local: usize, pane_cols: usize) -> usize {
        if pc_local.saturating_add(1) == pane_cols && self.wrap_paid_row == Some(pr) {
            0
        } else {
            pane_cols.saturating_sub(pc_local)
        }
    }

    /// THE COALESCED FOLD SHAPE (`shape_wrap`'s second arm, stated once for
    /// the classifier and the in-flight seam): two
    /// or three glyphs delivered across the fold in ONE observed move — down
    /// exactly one row, landing at the pane's columns 1..=3 (at least one
    /// glyph already on the new row, so a plain Enter to column 0 can only
    /// match the tight arm), from within four columns of the pane's edge,
    /// and on the alt screen only under a fresh repaint blink — the same
    /// discriminator `rainbow_coalesce` and the typed re-anchor take there
    /// (on the alt screen, where Claude Code and every measured shape run,
    /// a stalled batch's drain blinks its own bracket in the frame that
    /// judges it, and a blinkless cross-row hop there stays vim's
    /// relocation; a flat `!ctx_alt` made every such drain a landing-only
    /// re-anchor or a refused-and-forgotten hop). Returns the
    /// cells the fold lays — the origin row's tail plus the landing row's
    /// head, 2..=7 — or `None` off the shape. The classifier admits it
    /// under a live typing rhythm OR the in-flight licence; the seam admits
    /// it on the pool with exact payment.
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

    /// HOST KEY-HINT: one KILL keypress (Ctrl-K/U/W, Alt-D, word-delete
    /// Backspaces, forward Delete) — text is about to vanish in a span, not a
    /// glyph. Three arms in one note:
    /// - the KILL hint licenses the erase POOF: within [`Self::KILL_HINT_FRESH`]
    ///   a same-row NET SHRINK of the probed row content puffs smoke (or the
    ///   style's own sparkle/steam/droplet language) off the vanished span;
    /// - fire's QUENCH escalates like a big backspace ([`Self::KILL_QUENCH_GAIN`]
    ///   ≈ two deletes) and cools the standing heat/coal — killing a line
    ///   un-earns momentum and visibly douses the blaze at the same moment the
    ///   steam flashes;
    /// - moving-kill morphology retires older candidate classes. Without exact
    ///   content/target proof its later caret relocation stays dark.
    ///
    /// The `moves_cursor` flag says whether this kill normally relocates the
    /// caret. Ctrl-U/W and word-backspaces arm the bounded navigation classifier,
    /// but that timestamp is not provenance. A stationary kill (Ctrl-K, Alt-D,
    /// forward Delete) must not leak the classifier into the next exact typed
    /// candidate.
    pub fn note_kill(&mut self, now: Instant, moves_cursor: bool) {
        self.recent_typed_run = None;
        self.flush_held_park();
        if self.newline_hint.take().is_some() && self.v2.engaged() {
            self.v2.on_event(rk::Event::CancelComposerNewline, now);
        }
        self.unsettle();
        // A kill is the line's content going (the in-flight law): whatever
        // presses were still in flight on it are forgotten
        // at the key, as the echo ledger forgets them on its `Kill`.
        self.forget_typed_credits(now);
        // Word-Backspace reaches the host's generic Backspace arm first, then
        // this stronger classification at the same timestamp. Last arm wins:
        // the actual kill keeps its permissive fallback and swoosh.
        self.bs_poof_hint = None;
        self.kill_hint = Some(now);
        self.quench = (self.quench + Self::KILL_QUENCH_GAIN).min(1.0);
        self.heat *= Self::QUENCH_COOL;
        self.coal *= Self::QUENCH_COOL;
        // Canonical metric: a span erased un-earns like ~two deletes (the
        // same escalation the quench above documents) — and never builds.
        self.momentum.kill(now);
        // The MIRROR metric builds — and a span kill is a bigger gesture than a
        // character, so it credits TWICE, exactly the escalation the forward
        // metric spends on a kill. One `advance` per erase keeps the two laws
        // symmetric; the second is the span's extra weight.
        self.erase_mom.advance(now);
        self.erase_mom.advance(now);
        // Stamp the PRE-kill row exactly as `note_backspace` does — the two poof
        // licences share every downstream gate, so they must share the witness
        // too. This one is read ONLY by `poof_erase_witnessed` (the fallback's
        // early release); the plain-Backspace `bs_erased` law cannot see a
        // kill-stamped value, because the only thing that arms `bs_poof_hint`
        // is `note_backspace`, which re-stamps this field unconditionally.
        self.bs_baseline = self.row_fill_at_key();
        // A caret-MOVING kill (Ctrl-U/W, word-backspace) is licensed by the
        // nav stamp it arms here. A stationary kill (Ctrl-K, Alt-D, forward
        // Delete) moves no caret at all, so `spawn` never runs for it and its
        // poof is minted by `tick`'s erase detector, not by a move.
        if moves_cursor {
            self.nav_hint = Some(now);
        }
        // …and it owes exactly ONE such retreat, dated here. See
        // `kill_retreat_pending`.
        self.kill_retreat_pending = moves_cursor.then_some(now);
        // (`kill_reported_to_v2` is keyed on this stamp and needs no reset:
        // a kill already reported stays reported, a new kill is a new key.)
        // A fresh licence is a fresh key: only `note_word_kill` may mark the
        // hint word-scale, and it does so AFTER this runs.
        self.kill_hint_word = false;
    }

    /// [`Self::note_kill`] for a WORD-scale kill chord (^W, Alt-D, Alt/Ctrl-
    /// Backspace): everything about the licence is the line kill's — same
    /// freshness, same witnesses, same downstream gates — but the poof it
    /// licenses speaks [`crate::trail_sound::SoundKind::KillWord`], the erase
    /// poof's slightly larger, softer cousin, instead of the tier-3 swoosh: a
    /// word leaving is one word going, not a clause (owner, 2026-08-29).
    pub fn note_word_kill(&mut self, now: Instant, moves_cursor: bool) {
        self.note_kill(now, moves_cursor);
        self.kill_hint_word = true;
    }

    /// DISARM a dangling typed/backspace hint. Called when a key with its OWN
    /// move semantics arrives (Enter, Tab, nav, kill): a hint left over from a
    /// no-move echo (a password prompt swallowing the char, vim `x`/`r`) must
    /// not re-anchor the next independently proven move.
    /// The BACKSPACE pairing hint (`quench_hint`) is cleared too, because
    /// `bs_pair` in `spawn` only PEEKS it: a quench hint left by a backspace
    /// whose echo never moved (start of line, or a TUI coalescing the erase into
    /// the next repaint) would otherwise survive into a following Ctrl-A/E press
    /// and misclassify a later exact candidate. A content-confirmed deletion
    /// consumes the hint at its own echo, before any later navigation press.
    ///
    /// THIS IS THE CLASS-CHANGING PRESS-PATH SUPERSEDE. The GUI host calls it
    /// on every NON-typed press (and input boundary — mouse, focus, paste
    /// drain) before class-specific code arms, so a typed stamp whose echo
    /// never arrived cannot license whatever a chord, click or nav key moves
    /// next.
    ///
    /// A TYPED press no longer comes through here — it goes through
    /// [`Self::supersede_typed_press`], which closes every OTHER license term
    /// but keeps the banked typed stamps. The old regime ("every newer press
    /// closes the older license term", one keystroke per rendered frame) wiped
    /// the previous typed stamp on every keypress, so whenever keys outran
    /// frames the surplus echo sweeps were declined at the no-fresh-hint gate
    /// and printed as permanent background-black holes in the ribbon —
    /// measured on-glass at 5-13 declined cells per 100-key flood (dark%
    /// 11.5-13.2, the owner's-screenshot shape). Typed stamps are banked per
    /// press in [`TypedStamps`] and spent one per observed echo sweep, so the
    /// ceiling is gone: K keys in one frame license K sweeps, no more.
    pub fn clear_typed(&mut self, now: Instant) {
        // This closes movement licences, including the boundary between two
        // controller `turn` calls. The recent typed run is content evidence,
        // not an unpaid licence: a later typed echo must still prove its own
        // credit and an exact, adjacent same-row continuation before rejoining.
        // Explicit edit/navigation/geometry boundaries retire the run where
        // they are classified, rather than every generic licence clear.
        self.flush_held_park();
        if self.newline_hint.is_some() && self.v2.engaged() {
            self.v2.on_event(rk::Event::CancelComposerNewline, now);
        }
        self.type_hint.clear();
        self.clear_non_typed_hints(now);
    }

    /// The non-typed half of [`Self::clear_typed`] — every one-shot class,
    /// closed by a press that is a class change (an arrow, a chord). The
    /// typed press's supersede ([`Self::supersede_typed_press`]) closes
    /// less: a glyph contradicts no one-shot whose echo is still owed.
    pub(super) fn clear_non_typed_hints(&mut self, now: Instant) {
        self.user_gesture_hint = None;
        self.nav_hint = None;
        self.return_hint = None;
        self.reflow_hint = None;
        self.newline_hint = None;
        // The quench hint is dropped only once its OWN echo had a fair chance
        // to land (~2 frames): a fast backspace→Enter pair must not lose the
        // deletion steam to a disarm racing the echo.
        if self
            .quench_hint
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() > 0.03)
        {
            self.quench_hint = None;
        }
    }

    /// THE TYPED-PRESS SUPERSEDE — [`Self::clear_typed`]'s sibling for a press
    /// that is ITSELF a typed glyph: a glyph supersedes only what it
    /// contradicts (the Tab / scripted gesture and the reflow) and nothing
    /// whose echo is still owed. The banked typed stamps survive (each a
    /// real key whose echo is still in flight; more typing is not a class
    /// change — wiping them here was half of the flood-typing black gap,
    /// the other half the 1-deep slot, see [`TypedStamps`]), and so do the
    /// Return, nav, quench and composer-newline one-shots: a glyph typed
    /// before the Enter's response, the arrow's hop, the Backspace's
    /// retreat or the Shift+Enter's line break lands (a slow prompt: SSH, a
    /// starship / p10k precmd; Claude Code's measured p99 input latency
    /// 268 ms) is type-ahead onto the new row. With those one-shots
    /// dropped, their late echo was judged under the glyph's fresh stamp — a
    /// one-row Return response became a typed re-anchor that lit the prompt
    /// cell and spent the glyph's credit, and the glyph's own +1 was refused
    /// dark. The host calls this instead of `clear_typed` on the
    /// printable-glyph arm (and ahead of `note_return` / `note_user_gesture`
    /// for Enter, Tab and ⌃V); [`Self::note_typed_glyph`] calls it before
    /// banking the new press's own stamp.
    pub fn supersede_typed_press(&mut self) {
        self.user_gesture_hint = None;
        self.reflow_hint = None;
    }

    /// Revoke the classifier cohort armed by the input dispatch at `at` — the
    /// license terms included, so a key that never reached the child cannot
    /// license a later program move.
    /// The host calls this when a key was queued behind a draining paste,
    /// an inline write failed, or a paste's arrival stamp must not outlive
    /// its enqueue (the write's completion re-arms the DELIVERED insert at
    /// its own instant — [`Self::note_insert_delivered`]). App
    /// input dispatch is one event-loop turn, so no effect tick can observe
    /// the transient arm between note and revoke; timestamp matching avoids
    /// cancelling a newer gesture — or that later delivery stamp — if this
    /// API is ever called from a delayed path.
    pub fn revoke_input_hints_at(&mut self, at: Instant) {
        let clear = |slot: &mut Option<Instant>| {
            if *slot == Some(at) {
                *slot = None;
            }
        };
        self.type_hint.revoke_at(at);
        // The press CREDIT stays: a key queued behind a draining paste WILL
        // reach the wire (the FIFO writes it, in order, after the paste),
        // so its press is in flight in the truest sense — its echo is
        // payable by the pool, and its stamp is re-banked at the write's
        // completion ([`Self::note_delivered`], whose contract is that
        // credits and the ledger were never revoked). A key whose
        // write FAILED never reaches the child: that one is
        // [`Self::revoke_failed_input_at`], which takes the credit too.
        clear(&mut self.quench_hint);
        clear(&mut self.nav_hint);
        clear(&mut self.return_hint);
        clear(&mut self.user_gesture_hint);
        clear(&mut self.newline_hint);
        clear(&mut self.reflow_hint);
        clear(&mut self.kill_hint);
        // The delivered insert is revoked by its exact instant too — a Tab
        // or ⌃V queued behind a draining paste stamps at dispatch and is
        // revoked when the write did not happen inline; a delivery stamped
        // at a different instant is untouched, and one this arm had
        // ACCUMULATED into (an earlier paste's unspent credit) is restored,
        // not discarded with it — the queued Tab re-arms its own bound at
        // delivery and must not cost the paste its width. The arm's tally
        // goes with it: a revoked arm was not a delivery, so
        // its `inserts_delivered=` tick is undone — the re-arm at delivery
        // counts the gesture once (a failed write, which delegates here,
        // never re-arms and counts nothing) — and `last_insert_cells=`
        // reads as it did before the arm, not the width of an insert that
        // never went (`inserts_delivered=0 last_insert_cells=32` was the
        // shape after a queued Tab whose write then failed).
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

    /// Revoke the cohort of a dispatch whose inline write FAILED — the child
    /// never saw the key — [`Self::revoke_input_hints_at`] plus the PRESS
    /// CREDIT: a press the tty never saw is not in flight (the
    /// licence model's swallow disposition), and since the one-press echo a
    /// credit alone would license the next program `+1` on the row as
    /// `inflight`. A key merely QUEUED keeps its credit
    /// (`revoke_input_hints_at`): it will echo, and the pool must be able to
    /// pay for it.
    pub fn revoke_failed_input_at(&mut self, at: Instant) {
        self.revoke_input_hints_at(at);
        self.type_press_ring.revoke_at(at);
    }

    /// Consume every ONE-SHOT classifier at a completed hidden→visible
    /// boundary where `spawn` did not consume them. This includes a same-cell
    /// return: although no relocation occurred, the authored hide choreography
    /// is complete and its one-shot must not license a later PTY/CUP move.
    ///
    /// THE TYPED BANK IS SPARED WHILE FRESH: a TUI that brackets each repaint
    /// in DECTCEM-hide (Claude Code's per-keystroke choreography) completes
    /// a hidden boundary mid-burst whenever a frame catches the hidden
    /// phase, so stamps younger than [`Self::TYPE_HINT_FRESH`] — real keys
    /// whose echoes have not landed — survive it (wiping the bank orphaned
    /// every echo still in flight into a no-fresh-hint decline, a permanent
    /// unlit notch); stale ones are retired. The credit ring is bounded by
    /// [`IN_FLIGHT_PATIENCE_S`], not the stamp window: a boundary
    /// completing three seconds into a stall must spare the whole batch.
    /// The delivered insert and a held park follow the bank's law, spared
    /// while fresh. The one-shot class hints (quench/return/gesture/newline/
    /// reflow) clear unconditionally — each licenses exactly one move, and
    /// the hidden choreography WAS that move — EXCEPT a fresh nav hint: a
    /// same-cell hidden→visible completion under it is a key whose echo has
    /// NOT landed (the present that snapped a scrolled viewport back
    /// arrives before zsh's `\x08`×n, an Ink repaint's hidden half before
    /// its `CUP`), and wiping it declined that echo `no-fresh-hint` one
    /// frame later, one silent word hop apiece. A hint younger than
    /// [`Self::NAV_HINT_FRESH`] survives; a stale one retires, so nothing an
    /// unhinted program relocation can borrow changes.
    pub(super) fn retire_hidden_movement_provenance(&mut self, now: Instant) {
        // A HELD PARK IS SPARED WHILE FRESH, exactly like the typed bank it
        // stands on: Ink brackets its park and its rewrite in DECTCEM hides,
        // and a frame sampling the hidden caret in the ~40 ms between them
        // completes a boundary mid-park; flushed here, the park would be
        // judged as the re-anchor, the return refused and the credit
        // stranded. A park past its window is judged now, as the tick
        // would. The unconditional twin (`retire_all_movement_provenance`,
        // the declined relocation) still flushes: nothing about a refused
        // relocation is the park's return.
        if self.held_park.is_some_and(|p| !p.fresh(now)) {
            self.flush_held_park();
        }
        self.type_hint.retire_stale(now, Self::TYPE_HINT_FRESH);
        // THE DELIVERED INSERT FOLLOWS THE BANK'S LAW, not the one-shots':
        // Claude Code completes a hidden boundary mid-burst, and a delivered
        // insert whose echo is still in flight is a real gesture — spared
        // while fresh, retired once stale.
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
        // THE CREDIT RING KEEPS ITS OWN CLOCK: a credit is retired when its
        // cells are LAID or when it is past the in-flight patience, never at
        // `TYPE_HINT_FRESH` — Claude Code brackets every repaint in a DECTCEM
        // hide, so a boundary completes mid-batch, and a boundary completing
        // three seconds into a stall must spare the whole batch (credits
        // younger than the patience are real keys whose echoes have not
        // landed).
        self.type_press_ring.retire_stale(now);
        if self
            .last_committed_type
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() > Self::TYPE_HINT_FRESH)
        {
            self.last_committed_type = None;
        }
    }

    /// The unconditional twin of [`Self::retire_hidden_movement_provenance`]
    /// for the UNSEEDED first-draw boundary: a fresh/reset engine has no
    /// truthful source cell, so even a fresh pre-first-draw stamp must not
    /// license the next unrelated CUP. Everything goes, bank and ring
    /// included — the pre-fix behavior, kept exactly where it was honest.
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

    /// A hidden→visible cursor relocation that the bounded ConPTY bridge
    /// declined never reaches `spawn`, so its one-shot classifiers would
    /// otherwise remain armed for the next unrelated move. For Rainbow Kitty
    /// the engine's caret mirror is moved to `cur`
    /// ([`rk::Engine::observe_caret`], itself row-change only), so the next
    /// key lays at the caret's true cell — **and nothing of the ribbon is
    /// retired here**: melting every live cell on the row the caret was
    /// last seen on ([`Self::last_visible`]) in 120 ms, whether or not that
    /// row's text had moved, cut — on the one gesture the owner makes most,
    /// Enter in Claude Code (the composer cleared inside a hide bracket, the
    /// caret shown again elsewhere) — a band whose text had simply gone:
    /// *"the contrail disappeared versus nicely fading"*. The content
    /// witness reads the abandoned row's glyphs on this very frame and
    /// decides, cell by cell: replaced or moved with the caret → the fast
    /// melt; gone → released to the swoosh; intact → nothing, the row lives
    /// its own life and swooshes. A declined relocation with the old row's
    /// glyphs intact therefore only moves the mirror. Same-row hidden warps
    /// were always the ledger's and the witness's; now every row change is
    /// the witness's too.
    pub(super) fn retire_declined_hidden_relocation(&mut self, cur: (u16, u16)) {
        // FULL consumption, deliberately NOT the fresh-sparing boundary
        // retire: this relocation was REFUSED (outside the bounded bridge),
        // so nothing about it is correlated with the banked keys — a spared
        // stamp here is exactly what the next one-cell program update would
        // borrow into the reported stray segment (see
        // `cold_program_motion_emits_no_rainbow_light`).
        self.retire_all_movement_provenance();
        // The caret has crossed to a row the prior row-content sample did not
        // observe. Retire kill/plain-Backspace proofs and both probe
        // generations so a reused numeric row cannot synthesize a later poof.
        self.drop_row_probe();
        self.crown_until = None;
        if self.v2.engaged() {
            self.v2.observe_caret(cur);
        }
    }
}
