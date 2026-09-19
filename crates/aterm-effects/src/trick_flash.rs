// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TRICK FLASH: the word a person just typed TO THE PET — `sit`, the `good`
//! of `good kitty` — turns rainbow for about a second and then hands the glyphs
//! back, byte for byte, in the colour they had.
//!
//! # Why this is not a lexicon class or a colorway
//!
//! Pet commands are ordinary verbs (`sit`, `play`, `roll`) that fill every
//! build log and README. As a scanned class they would cost an occurrence slot,
//! a persist episode and an ink capture for every ambient `run` on the screen,
//! and the existing rainbow colorway SETTLES to a permanent tint. So the screen
//! scanner never learns these words. The typed-line listener
//! ([`crate::typed_tricks`]) decides that a word was a command; the host tells
//! the word engine WHICH word and how far behind the caret it sits; and this
//! module does the rest with one slot: find that word on the glass, paint it,
//! let go.
//!
//! # The three laws
//!
//! * **TYPED, NEVER SEEN.** Nothing here reads the screen for vocabulary. A
//!   slot exists only because the input path noted one, so `echo sit`, a pasted
//!   `sit` and a `cat`ted file full of `sit` never ink: with no note there is
//!   no slot, and with no slot both hooks are one `Option` test.
//! * **NEVER PAINT A PLACE YOU HAVE NOT FOUND.** The note carries a caret and
//!   a word, not a cell. The word becomes paintable only when a rescan of the
//!   SAME pane finds those exact characters, as a whole word, on the row the
//!   caret stood on (moved by exactly the scroll the snapshot reports), within
//!   reach of that caret. A slot that is not found within
//!   `GIVE_UP_MS` is dropped: silence is the safe direction for an effect
//!   that claims "you typed this".
//! * **LEAVE NO RESIDUE.** The colour is `mix(base, rainbow, env(t) · strength)`
//!   and `env` is zero at both ends, so the last frames converge on the base
//!   and past the window the flash emits ZERO cells, forever. Where the word
//!   already carries occurrence ink (a custom spec, a feline self-glow) that
//!   ink IS the base, so the fade lands on exactly what would have shown
//!   without the flash.
//!
//! # Finding the word
//!
//! The grid does not hold what was typed. A cell holds its BASE scalar only
//! (zero-width marks ride the previous cell), so Thai `นั่ง` is the two cells
//! `น ง`; the shipping GUI lays an RTL run out in VISUAL order, so Hebrew `שב`
//! arrives reversed in place; and an East-Asian-ambiguous letter is two cells
//! wide under `ambiguous_width_double`. So the typed word is stored with its
//! zero-width scalars removed, an RTL word is also tried mirrored, and every
//! column comes from the matched cells, never from a width computed here.
//!
//! The ROW is the hard part. `sit⏎` on the bottom row scrolls before its echo
//! is ever scanned, and the first thing the scan then sees is the shell's own
//! `command not found: sit` one row above the typed line. Searching upward for
//! the nearest match would flash the shell's OUTPUT, which is the one thing a
//! typed-provenance effect must never do. The snapshot already carries the
//! exact answer — `base_y`, the absolute row of the top visible line — so a
//! host that declares it (`TrickFlash::declare_base_y`) gets
//! `row = caret.row − (base_y_now − base_y_then)` and ONLY that row is tested.
//! The bounded upward search survives only for a scroll the snapshot cannot
//! see (a scroll REGION: tmux, the alt screen), where the delta is zero and the
//! caret's row came up empty; it is nearest-first and takes EXACTLY anchored
//! candidates only — and only on the reading that says the word had ALREADY
//! echoed in full when the key was committed (a caret one past its end), and
//! only once the live cursor has left the place the key was committed at. A
//! row that comes up empty because the echo is merely late must not start a
//! search: all that search can find is last time's `$ sit`, or a `sit` some
//! program printed.
//!
//! # Scope
//!
//! Window-scoped and not parked with the panes: a keyboard has one owner, so
//! the slot is a claim about the person typing, and it names its pane IN the
//! value (the typed-edit witness precedent). Every hook compares that pane to
//! the scan scope, which is what keeps pane B from painting pane A's flash at
//! the same cell. The cardinality sentence lives on the owning field in
//! `word_decorations.rs`.
//!
//! # Flash safety
//!
//! Per-flash safety is STRUCTURAL: one hue revolution per flash, never faster
//! than the crate's animation floor, and the declared luminance-reversal rate
//! is const-asserted under the WCAG 2.3.1 bound this crate certifies (the
//! measured twin is `the_flash_stays_under_the_photosensitivity_bound`). The
//! start debounce below is cosmetic, not a limiter.
//!
//! # Cost
//!
//! No slot: one `Option` test per rescan and per tick, plus a probe of the
//! short base-y memo on a cursor-bearing rescan of a host that declares one. A
//! slot: one row copy per rescan (at most `SEARCH_UP_ROWS` more on the
//! scroll-region fallback) and at most 32 cells per frame for about a second.
//! Fixed inline buffers; the only heap is the memo's first few pushes. The
//! note, the revoke and a flashing frame allocate NOTHING, whatever the word
//! (pinned by `tests/trick_flash_alloc.rs`).

use std::time::Duration;

use aterm_core::terminal::RenderCell;
use aterm_lexicon::{TrickLexicon, is_no_space_script, is_token_char};
use aterm_render::InkCell;
use aterm_scene::{mix_rgb, smoothstep};
use aterm_time::Instant;

use crate::color_math::{hsv2rgb, relative_luminance};
use crate::effect_util::TWINKLE_FLASH_BOUND_HZ;
use crate::spec::{MIN_ANIMATION_MS, RAINBOW_SAT, RAINBOW_SPAN_DEG, RAINBOW_VAL};
use crate::word_decorations::{
    INK_FADE_MS, INK_RAMP_IN_MS, MIN_INK_CONTRAST, TYPED_EDIT_REACH_COLS, TYPED_EDIT_WITNESS_MS,
    fold_ink, fold_u64, rainbow_base_hue, rgb3_to_u32, u32_to_rgb3,
};

/// The whole flash, ms: ramp in, hold, fade out. Also the hue cycle's period —
/// ONE revolution per flash, so no cell crosses the yellow-to-blue luminance
/// cliff more than once. Crate-visible for the engine's hook tests, which pin
/// the deadline the tick arms.
pub(crate) const FLASH_MS: u32 = 1100;
/// Smooth ramp-in, ms (the ink sweep's own ramp).
const RAMP_MS: u32 = 130;
/// Smoothstep fade over the LAST this-many ms, back to the base colour.
const FADE_MS: u32 = 250;
/// A revoked flash (the line turned out to be prose) leaves over this many ms
/// instead of finishing its second.
const REVOKE_FADE_MS: u32 = 120;
/// How long a noted word may stay unfound. The same clock-skew budget as the
/// typed-edit witness: the key is committed at input time and the word becomes
/// scannable only when the shell's echo lands.
const GIVE_UP_MS: u64 = TYPED_EDIT_WITNESS_MS;
/// The scroll-region fallback looks at most this many rows above the caret.
const SEARCH_UP_ROWS: usize = 12;
/// Peak tint toward the rainbow before the legibility guard. The flash OWNS
/// this (it never reads the shared ink strength), so the Settings notes about
/// which effects consume that knob stay true.
const FLASH_STRENGTH: f32 = 0.9;
/// The typed word's inline buffer IS the listener's token window: a word the
/// vocabulary could hold always fits, and a longer one is not a trick.
const MAX_CHARS: usize = TrickLexicon::MAX_SURFACE_CHARS;
/// Panes whose last cursor-bearing `base_y` is remembered. A window shows a
/// handful; past the cap the oldest entry leaves, which costs that pane one
/// flash's scroll-following and nothing else.
const BASE_Y_MEMO_CAP: usize = 16;

/// Large luminance reversals one flash can show on one cell. Its luminance
/// makes at most four big moves in a row — off the base on the ramp, across the
/// hue revolution's two cliffs (up to yellow, down to blue), and back to the
/// base on the fade — which is at most THREE changes of direction. Declared
/// here; measured off the real colour law, and found tight, by the test named
/// in the module docs.
const FLASH_REVERSALS: f32 = 3.0;
/// That count as a rate over the flash, Hz.
const FLASH_REVERSAL_HZ: f32 = FLASH_REVERSALS * 1000.0 / FLASH_MS as f32;

/// A PHOTOSENSITIVITY BOUND IS NOT A TEST CASE (the `effect_util` law): retune
/// a timing past any of these and the crate stops building. The hue cycle may
/// not run faster than the crate's animation floor; the ramp and the fade may
/// not be sharper than the ink sweep's own; the envelope must have room for
/// both; a revoke may hurry the fade but never to a cut; and the declared
/// reversal rate stays under the general-flash bound.
const _: () = assert!(
    FLASH_MS >= MIN_ANIMATION_MS
        && RAMP_MS as u64 >= INK_RAMP_IN_MS
        && FADE_MS as u64 >= INK_FADE_MS
        && RAMP_MS + FADE_MS <= FLASH_MS
        && REVOKE_FADE_MS > 0
        && REVOKE_FADE_MS <= FADE_MS
        && FLASH_REVERSAL_HZ <= TWINKLE_FLASH_BOUND_HZ,
    "the trick flash's timings left the structural flash-safety envelope"
);

/// Where the one flash slot stands at `now` — the lifecycle the formal model
/// binds, readable without touching the glass.
///
/// A pure function of the slot and the clock: a pending slot past its give-up
/// bound and a live one past its window already read [`Idle`](Self::Idle),
/// whether or not a rescan or a tick has come by to collect them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrickFlashPhase {
    /// No flash: never noted, finished, given up, dropped, or revoked and gone.
    Idle,
    /// Noted by the input path and not yet found on the glass. Paints nothing.
    Pending,
    /// Found; the envelope is running.
    Live,
    /// Found and then revoked; the short fade is running.
    Revoked,
}

/// One noted word: what was typed, where the hands were, and — once a rescan
/// has found it — where it is.
#[derive(Clone, Copy, Debug)]
struct Slot {
    /// When the boundary key was committed (host input time).
    at: Instant,
    /// The pane the key went to; `None` from a host that names none.
    pane: Option<u64>,
    /// The PRE-ECHO caret of that pane, `(row, col)`.
    caret: (u16, u16),
    /// That pane's `base_y` at the scan which recorded the caret, when the
    /// host declares one.
    base_y: Option<i64>,
    /// The typed word, zero-width scalars removed, in LOGICAL order.
    chars: [char; MAX_CHARS],
    len: u8,
    /// Typed characters between the word and the caret (0 for a plain fire).
    back_chars: u16,
    /// The word holds a strong-RTL scalar: also try it mirrored.
    rtl: bool,
    /// Decides the flash's opening hue.
    hue_seed: u64,
    located: Option<Located>,
    /// Set once by a revoke on a located slot; never moved by a second one.
    revoked_at: Option<Instant>,
}

/// Where a slot's word was found, and everything the emitter needs so that a
/// frame reads no grid at all.
#[derive(Clone, Copy, Debug)]
struct Located {
    /// The envelope's zero: `max(rescan clock, key time)`. NOT the key time —
    /// a late echo would otherwise skip the ramp and pop in at full rainbow.
    since: Instant,
    row: u16,
    base_y: Option<i64>,
    /// The grid width it was found under; a different width ends the flash.
    grid_cols: u16,
    /// The match was the mirrored (visual-order) spelling.
    mirrored: bool,
    /// LEAD cells only, ascending: a wide glyph's lead governs both halves.
    cols: [u16; MAX_CHARS],
    fg: [[u8; 3]; MAX_CHARS],
    /// Resolved for the word's background (light backgrounds take deep tones).
    sat: f32,
    val: f32,
    /// The legibility-guarded peak tint, computed at locate time and NOT per
    /// frame: a per-frame guard steps in eighths as the hue rotates through
    /// yellow and blue, which is visible inside a one-second pulse.
    strength: f32,
}

/// The flash state a word engine owns. Free-standing on purpose: every method
/// takes the scan scope, the caret and the cells BY ARGUMENT, so the engine's
/// own `impl` stays in its own file.
#[derive(Debug, Default)]
pub(crate) struct TrickFlash {
    slot: Option<Slot>,
    /// When the last flash became visible — the cosmetic debounce's clock. It
    /// outlives the slot so revoke-then-refire cannot strobe either.
    last_start: Option<Instant>,
    /// What the host declared for its NEXT rescan, and for WHICH scan scope.
    /// Scope-keyed so one pane's declaration can never stand in for another
    /// pane's rescan, and taken by the rescan that reads it so one snapshot's
    /// row origin can never stand in for a later snapshot's either.
    declared: Option<(Option<u64>, i64)>,
    /// Per-pane `base_y` as of that pane's last CURSOR-BEARING rescan — the
    /// same moment its caret is recorded, so the pair a note reads is one
    /// observation.
    base_y: Vec<(Option<u64>, i64)>,
}

impl TrickFlash {
    /// The host's declaration for the rescan it is about to run in `scope` —
    /// that ONE rescan, which spends it. `None` withdraws it: the engine then
    /// treats every scroll delta as zero.
    pub(crate) fn declare_base_y(&mut self, scope: Option<u64>, base_y: Option<i64>) {
        self.declared = base_y.map(|y| (scope, y));
    }

    /// Forget panes the window no longer shows: their memo entries, and a slot
    /// that names one (no rescan of it will ever come to collect it).
    pub(crate) fn retain_panes(&mut self, keep: impl Fn(u64) -> bool) {
        self.base_y.retain(|(pane, _)| pane.is_none_or(&keep));
        if self
            .slot
            .is_some_and(|slot| slot.pane.is_some_and(|pane| !keep(pane)))
        {
            self.slot = None;
        }
    }

    /// Drop the flash. Idempotent, so the engine's per-pane reset fold may
    /// call it once per pane. The debounce clock and the memo are observations
    /// of the keyboard and the grid, not paint state, and stay.
    pub(crate) fn clear(&mut self) {
        self.slot = None;
    }

    /// The input path says `word` was just typed to the pet in `pane`, with
    /// the pre-echo `caret` and `back_chars` typed characters between the word
    /// and that caret. `scope` is the engine's current scan scope, which is
    /// the memo key for a host that names no pane.
    ///
    /// FAIL-SILENT: no caret observation, an empty or over-long word, or a
    /// flash that only just started all store nothing. O(len) into a fixed
    /// inline buffer; never allocates.
    pub(crate) fn note(
        &mut self,
        now: Instant,
        pane: Option<u64>,
        scope: Option<u64>,
        caret: Option<(u16, u16)>,
        word: &str,
        back_chars: u16,
    ) {
        // A pane the engine has never scanned with a cursor has no caret to
        // anchor on. The typed-edit witness ABSTAINS here; a paint decision
        // must not — abstaining would mean searching the whole screen.
        let Some(caret) = caret else {
            return;
        };
        // COSMETIC DEBOUNCE. A flash that is still in its opening third of a
        // second is not cut off by the next one: the new word is skipped. Not
        // a safety limiter — see the const asserts for that.
        if self.last_start.is_some_and(|started| {
            now.saturating_duration_since(started)
                < Duration::from_millis(u64::from(MIN_ANIMATION_MS))
        }) {
            return;
        }
        let mut chars = ['\0'; MAX_CHARS];
        let mut len = 0usize;
        let mut rtl = false;
        for c in word.chars() {
            // The grid keeps a zero-width scalar on the PREVIOUS cell, so it
            // can never be found as a cell of its own.
            if aterm_grapheme::char_width(c) == 0 {
                continue;
            }
            if len == MAX_CHARS {
                return;
            }
            chars[len] = c;
            len += 1;
            rtl |= is_strong_rtl(c);
        }
        if len == 0 {
            return;
        }
        let base_y = self
            .base_y
            .iter()
            .find(|(key, _)| *key == pane.or(scope))
            .map(|(_, y)| *y);
        self.slot = Some(Slot {
            at: now,
            pane,
            caret,
            base_y,
            chars,
            len: len as u8,
            back_chars,
            rtl,
            hue_seed: hue_seed(caret, &chars[..len]),
            located: None,
            revoked_at: None,
        });
    }

    /// The line turned out not to be pet talk. A flash on the glass leaves
    /// over [`REVOKE_FADE_MS`]; one that was never found is simply dropped.
    /// Idempotent: a revoke with nothing to revoke, or a second revoke, is a
    /// no-op (the first stamp stands, so the fade cannot be restarted).
    pub(crate) fn revoke(&mut self, now: Instant) {
        let Some(slot) = self.slot.as_mut() else {
            return;
        };
        match slot.located {
            None => self.slot = None,
            Some(loc) => {
                if slot.revoked_at.is_none() {
                    slot.revoked_at = Some(now.max(loc.since));
                }
            }
        }
    }

    /// See [`TrickFlashPhase`].
    pub(crate) fn phase(&self, now: Instant) -> TrickFlashPhase {
        let Some(slot) = self.slot.as_ref() else {
            return TrickFlashPhase::Idle;
        };
        match slot.located {
            None if millis_since(now, slot.at) > GIVE_UP_MS => TrickFlashPhase::Idle,
            None => TrickFlashPhase::Pending,
            Some(loc) if millis_since(now, loc.since) >= window_ms(&loc, slot.revoked_at) => {
                TrickFlashPhase::Idle
            }
            Some(_) if slot.revoked_at.is_some() => TrickFlashPhase::Revoked,
            Some(_) => TrickFlashPhase::Live,
        }
    }

    /// The rescan half, called by BOTH rescan twins after their scan pass:
    /// remember this pane's `base_y`, then locate a pending slot or re-verify
    /// a located one. `load_row` fills `row_buf` with one viewport row (the
    /// dense-scan idiom, so the term walk and the snapshot share this body).
    ///
    /// `paintable` is `ink on && !reduced motion`: when false the slot is
    /// dropped, because a flash that may not paint has nothing to wait for.
    #[allow(
        clippy::too_many_arguments,
        reason = "the rescan hands over its scope, clock, geometry, caret, gate and row source in one call; a carrier struct would rename the list, not shrink it"
    )]
    pub(crate) fn on_rescan(
        &mut self,
        scope: Option<u64>,
        now: Instant,
        rows: usize,
        cols: usize,
        cursor: Option<(u16, u16)>,
        paintable: bool,
        row_buf: &mut Vec<RenderCell>,
        mut load_row: impl FnMut(usize, &mut Vec<RenderCell>),
    ) {
        // SPENT HERE, whichever scope it was made for: a declaration describes
        // ONE snapshot. A later rescan the host did not declare for must read
        // as "no row origin" — not as the last one, which would pair the caret
        // this rescan records with a `base_y` from before the scroll.
        let base_y_now = self
            .declared
            .take()
            .and_then(|(declared_for, y)| (declared_for == scope).then_some(y));
        if cursor.is_some() {
            self.remember_base_y(scope, base_y_now);
        }
        // THE ONE TEST an engine with no flash pays.
        let Some(mut slot) = self.slot else {
            return;
        };
        if !pane_matches(slot.pane, scope) {
            return;
        }
        // A rescan of the flash's pane WITHOUT a cursor is a scrollback view:
        // viewport rows are no longer live rows, so every row this slot knows
        // is stale. Ending is the honest answer for a pending slot too.
        let (true, Some(cursor)) = (paintable, cursor) else {
            self.slot = None;
            return;
        };
        let grid_cols = u16::try_from(cols).unwrap_or(u16::MAX);
        self.slot = match slot.located {
            Some(loc) => follow(
                &slot,
                loc,
                base_y_now,
                rows,
                grid_cols,
                row_buf,
                &mut load_row,
            )
            .map(|loc| {
                slot.located = Some(loc);
                slot
            }),
            None if millis_since(now, slot.at) > GIVE_UP_MS => None,
            None => match locate(
                &slot,
                now,
                cursor,
                base_y_now,
                rows,
                grid_cols,
                row_buf,
                &mut load_row,
            ) {
                Some(loc) if loc.strength > 0.0 => {
                    slot.located = Some(loc);
                    self.last_start = Some(loc.since);
                    Some(slot)
                }
                // Found, but no tint at all would be legible on it: there is
                // nothing to show, so there is no flash (and nothing arms).
                Some(_) => None,
                None => Some(slot),
            },
        };
    }

    /// The tick half: merge this frame's flash cells into `ink`, keeping it
    /// sorted-unique by `(row, col)`, fold them and the flash clock into `fp`,
    /// and return the instant the flash ends so the caller can arm ITS OWN
    /// deadline. `None` means nothing was painted: `ink` and `fp` are exactly
    /// as they were handed in, which is what keeps the no-flash paths verbatim.
    ///
    /// Where `ink` already holds a cell, that cell's colour is the mix base,
    /// so the fade lands on what would have shown without the flash.
    pub(crate) fn emit(
        &mut self,
        scope: Option<u64>,
        now: Instant,
        paintable: bool,
        ink: &mut Vec<InkCell>,
        fp: &mut u64,
    ) -> Option<Instant> {
        // THE ONE TEST a frame with no flash pays.
        let slot = self.slot?;
        if !pane_matches(slot.pane, scope) {
            return None;
        }
        if !paintable {
            self.slot = None;
            return None;
        }
        // NEVER PAINT A PLACE YOU HAVE NOT FOUND.
        let loc = slot.located?;
        let t = millis_since(now, loc.since);
        let window = window_ms(&loc, slot.revoked_at);
        if t >= window {
            // Done: zero cells from here on, and the slot is collected.
            self.slot = None;
            return None;
        }
        let revoked_t = slot.revoked_at.map(|at| millis_since(at, loc.since));
        let tint = envelope(t, revoked_t) * loc.strength;
        let n = usize::from(slot.len);
        let hue0 = rainbow_base_hue(slot.hue_seed);
        for i in 0..n {
            let target = hsv2rgb(flash_hue(hue0, i, n, t), loc.sat, loc.val);
            let col = loc.cols[i];
            let at = ink.binary_search_by_key(&(loc.row, col), |c| (c.row, c.col));
            let cell = match at {
                Ok(j) => {
                    ink[j].color = u32_to_rgb3(mix_rgb(rgb3_to_u32(ink[j].color), target, tint));
                    ink[j]
                }
                Err(j) => {
                    let cell = InkCell {
                        row: loc.row,
                        col,
                        color: u32_to_rgb3(mix_rgb(rgb3_to_u32(loc.fg[i]), target, tint)),
                    };
                    ink.insert(j, cell);
                    cell
                }
            };
            *fp = fold_ink(*fp, &cell);
        }
        // The flash is animating for as long as it emits at all, and the
        // wordless-screen hook has no frame counter to fold: the flash clock
        // is what keeps the repaint early-out from skipping a live frame.
        *fp = fold_u64(*fp, t);
        Some(loc.since + Duration::from_millis(window))
    }

    fn remember_base_y(&mut self, scope: Option<u64>, base_y: Option<i64>) {
        let at = self.base_y.iter().position(|(key, _)| *key == scope);
        match (at, base_y) {
            (Some(i), Some(y)) => self.base_y[i].1 = y,
            // The host stopped declaring: a stale row origin would be worse
            // than none.
            (Some(i), None) => {
                self.base_y.remove(i);
            }
            (None, Some(y)) => {
                if self.base_y.len() >= BASE_Y_MEMO_CAP {
                    self.base_y.remove(0);
                }
                self.base_y.push((scope, y));
            }
            (None, None) => {}
        }
    }
}

/// What decides a flash's opening hue: the word and where it was typed, and
/// nothing else — no clock and no counter, so a replay paints the same bytes.
fn hue_seed(caret: (u16, u16), chars: &[char]) -> u64 {
    let at = fold_u64(
        fold_u64(0xcbf2_9ce4_8422_2325, u64::from(caret.0)),
        u64::from(caret.1),
    );
    chars.iter().fold(at, |h, c| fold_u64(h, u64::from(*c)))
}

/// The pane test every hook shares, and the typed-edit witness's own: a `None`
/// on either side names no pane at all (a host with one grid, or an engine
/// nobody declared a session to), so there is no divider for the flash to
/// cross. Two NAMED panes must be the same pane.
fn pane_matches(flash_pane: Option<u64>, scope: Option<u64>) -> bool {
    match (flash_pane, scope) {
        (Some(typed_in), Some(scanning)) => typed_in == scanning,
        _ => true,
    }
}

/// `now − then` in ms, zero when `now` is the earlier one: the introspection
/// capture path can legitimately hand the engine a rewound clock.
fn millis_since(now: Instant, then: Instant) -> u64 {
    now.saturating_duration_since(then).as_millis() as u64
}

/// How long a located flash lasts, ms from `since`: the whole window, or —
/// revoked — the short fade from the revoke, whichever ends first.
fn window_ms(loc: &Located, revoked_at: Option<Instant>) -> u64 {
    let whole = u64::from(FLASH_MS);
    revoked_at.map_or(whole, |at| {
        (millis_since(at, loc.since) + u64::from(REVOKE_FADE_MS)).min(whole)
    })
}

/// The tint envelope at `t` ms: smooth ramp in, hold, smoothstep fade over the
/// last [`FADE_MS`] — zero at both ends, which is the no-residue law. A revoke
/// at `revoked_t` multiplies in its own short fade; the product is continuous
/// at the revoke, so revoking never pops.
fn envelope(t: u64, revoked_t: Option<u64>) -> f32 {
    let t = t as f32;
    let ramp = smoothstep(t / RAMP_MS as f32);
    let fade = 1.0 - smoothstep((t - (FLASH_MS - FADE_MS) as f32) / FADE_MS as f32);
    let revoke = revoked_t.map_or(1.0, |r| {
        1.0 - smoothstep((t - r as f32) / REVOKE_FADE_MS as f32)
    });
    ramp * fade * revoke
}

/// Lead cell `i` of `n`'s hue at `t` ms, degrees: the opening hue, the word's
/// spatial spread (the rainbow colorway's span clamp — a readable two-step on
/// a 2-cell word, a pure temporal sweep on one cell), plus exactly ONE
/// revolution over the flash.
fn flash_hue(hue0: f32, i: usize, n: usize, t: u64) -> f32 {
    let steps = n.saturating_sub(1) as f32;
    let span = RAINBOW_SPAN_DEG.min(100.0 * steps);
    let u = i as f32 / steps.max(1.0);
    (hue0 + u * span + 360.0 * (t as f32 / FLASH_MS as f32)).rem_euclid(360.0)
}

/// The rainbow colorway's theme rule: a light background takes deep candy
/// tones that can clear the contrast bound on white.
fn resolve_sat_val(bg: u32) -> (f32, f32) {
    if relative_luminance(bg) > 0.5 {
        (1.0, 0.62)
    } else {
        (RAINBOW_SAT, RAINBOW_VAL)
    }
}

/// The legibility guard: the strongest tint, stepping down from
/// [`FLASH_STRENGTH`] in eighths, at which the flash is legible at EVERY hue
/// it will pass through.
///
/// WHY SIX SAMPLES ARE THE WHOLE CIRCLE. Between two multiples of 60° exactly
/// one RGB channel moves, linearly; the mix toward `fg` is linear per channel;
/// and relative luminance is monotone in each channel. So the tinted colour's
/// luminance is monotone on every 60° arc and its extremes over the circle sit
/// at the six corners. A coarser even grid (45°) misses both corners that
/// actually bind — pure blue on a dark theme, pure yellow on a light one.
///
/// Two conditions per corner. CONTRAST: at least [`MIN_INK_CONTRAST`] against
/// the word's background, the bound every ink holds. SAME SIDE: the tint stays
/// on the text's side of the background's luminance; two corners passing on
/// OPPOSITE sides would put a hue between them exactly on the background —
/// contrast 1.0, mid-rotation.
///
/// ZERO is a real answer, and the ink guard's own: text the theme itself draws
/// below the bound (dim comment grey, concealed `fg == bg`) cannot be tinted
/// through blue without losing what legibility it has, so it is not tinted at
/// all. The caller then ENDS the flash instead of arming a second of frames
/// that would show nothing — which also means a flash never reveals concealed
/// text.
fn legible_strength(fg: u32, bg: u32, sat: f32, val: f32) -> f32 {
    let lum_bg = relative_luminance(bg);
    let contrast = |lum: f32| (lum.max(lum_bg) + 0.05) / (lum.min(lum_bg) + 0.05);
    let floor = MIN_INK_CONTRAST;
    let text_is_lighter = relative_luminance(fg) >= lum_bg;
    let corners = [0.0f32, 60.0, 120.0, 180.0, 240.0, 300.0].map(|hue| hsv2rgb(hue, sat, val));
    let step = FLASH_STRENGTH / 8.0;
    let mut strength = FLASH_STRENGTH;
    'guard: while strength > 0.0 {
        for corner in corners {
            let lum = relative_luminance(mix_rgb(fg, corner, strength));
            if contrast(lum) < floor || (lum >= lum_bg) != text_is_lighter {
                strength = (strength - step).max(0.0);
                continue 'guard;
            }
        }
        break;
    }
    strength
}

/// A scalar that sits INSIDE a word on the glass: a token character of a
/// spaced script, or any no-space-script scalar. A match bounded by one of
/// these on either side is a piece of a longer word (`sit` in `site`).
fn is_word_char(c: char) -> bool {
    is_token_char(c) || is_no_space_script(c)
}

/// Strong right-to-left scalars: Hebrew, Arabic and their neighbours in the
/// BMP, the presentation forms, and the RTL planes of the SMP. Only decides
/// whether the mirrored spelling is ALSO tried, so a generous range is safe.
fn is_strong_rtl(c: char) -> bool {
    matches!(c as u32,
        0x0590..=0x08FF       // Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan, Mandaic, Arabic Ext
        | 0xFB1D..=0xFDFF     // Hebrew + Arabic presentation forms A
        | 0xFE70..=0xFEFF     // Arabic presentation forms B
        | 0x1_0800..=0x1_0FFF // Cypriot … Old Hungarian, Hanifi Rohingya, Sogdian
        | 0x1_E800..=0x1_EFFF // Mende Kikakui, Adlam, Arabic mathematical
    )
}

/// One whole-word match on a row.
#[derive(Clone, Copy)]
struct Hit {
    /// LEAD columns of the matched scalars, ascending.
    cols: [u16; MAX_CHARS],
    /// The caret sits exactly where the typed characters put it.
    exact: bool,
}

/// The best whole-word occurrence of `seq` in `cells` within reach of the
/// caret: EXACT anchors before tolerant ones, and among equals the RIGHTMOST
/// (the word just typed is the last thing on its line).
///
/// The row stream is `scan_row`'s: continuation halves of wide glyphs carry no
/// scalar and are skipped, and every column comes from the cells.
///
/// REACH is the typed-edit witness's span reach — the caret may be at the
/// word's start (an IME commit, a burst that outran its echo), inside it
/// (lag), or past it (Space, Enter, a punctuation tail) — widened by
/// `2·back_chars + 2` columns, because a held address word sits a few typed
/// characters away and each of those is at most two cells. It is SYMMETRIC:
/// a word inside one IME commit lies to the RIGHT of the recorded caret.
///
/// EXACT means the distance is what `back_chars` narrow or wide characters
/// make it: `caret == end + 1 + w·back` (typed, word on the LEFT) or
/// `caret + w·back == start` (one commit, word on the RIGHT), `w ∈ {1, 2}`.
/// With nothing in between that is `end + 1 == caret` or `start == caret`.
///
/// `exact_only` is the upward search, and it is stricter still: it takes the
/// LEFT reading alone. The search FOLLOWS a line that moved, so it may only
/// look for a word the engine had already seen in full — and a caret recorded
/// one past the word's end is the proof of that. The RIGHT reading says the
/// opposite: the caret stood where the word was GOING to start, none of it had
/// echoed, and the caret's row coming up empty means "not yet", not "gone".
/// Searching on that reading finds only history, and finds it reliably: last
/// time's `$ sit` starts at the very same prompt column (a burst from `aterm
/// ctl key`, or a slow link, split across two frames). A line only scrolls
/// because Enter was typed and a commit never carries an Enter, so nothing an
/// IME produces is lost; a burst inside a scroll region is — silently. Every
/// extra reading is also one more column at which the shell's own `command not
/// found: good` lines up by coincidence (measured: `$ good kitty⏎` at a
/// two-column prompt puts that `good` at exactly `caret + 2·6`).
fn find_in_row(
    cells: &[RenderCell],
    seq: &[char],
    caret_col: u16,
    back_chars: u16,
    exact_only: bool,
) -> Option<Hit> {
    let (first, caret, back) = (*seq.first()?, i64::from(caret_col), i64::from(back_chars));
    let reach = i64::from(TYPED_EDIT_REACH_COLS) + 2 * back + 2;
    let mut best: Option<Hit> = None;
    for start in 0..cells.len() {
        if cells[start].wide || cells[start].ch != first {
            continue;
        }
        // Left bound: the previous LEAD cell (step over a continuation half).
        let before = cells[..start].iter().rev().find(|cell| !cell.wide);
        if before.is_some_and(|cell| is_word_char(cell.ch)) {
            continue;
        }
        let mut cols = [0u16; MAX_CHARS];
        let mut at = start;
        let mut matched = true;
        for (k, want) in seq.iter().enumerate() {
            while cells.get(at).is_some_and(|cell| cell.wide) {
                at += 1;
            }
            match (cells.get(at), u16::try_from(at)) {
                (Some(cell), Ok(col)) if cell.ch == *want => cols[k] = col,
                _ => {
                    matched = false;
                    break;
                }
            }
            at += 1;
        }
        if !matched {
            continue;
        }
        // `at` is one past the last lead; its continuation half, if any, is
        // still part of the word's on-screen span.
        while cells.get(at).is_some_and(|cell| cell.wide) {
            at += 1;
        }
        if cells.get(at).is_some_and(|cell| is_word_char(cell.ch)) {
            continue;
        }
        let (start, end) = (start as i64, at as i64 - 1);
        if caret < start - reach || caret > end + 1 + reach {
            continue;
        }
        let typed_left = (1..=2).any(|w| caret == end + 1 + w * back);
        let committed_right = (1..=2).any(|w| caret + w * back == start);
        if exact_only && !typed_left {
            continue;
        }
        let exact = typed_left || committed_right;
        // Left-to-right walk: `>=` on the exactness rank keeps the rightmost.
        if best.is_none_or(|b| exact >= b.exact) {
            best = Some(Hit { cols, exact });
        }
    }
    best
}

/// [`find_in_row`] for the typed spelling and, for an RTL word, its mirror
/// image — the shipping GUI delivers an RTL run reversed IN PLACE, so the
/// columns are the same and only the scalar order differs. Returns the hit and
/// whether it was the mirrored one.
fn find_word(slot: &Slot, cells: &[RenderCell], exact_only: bool) -> Option<(Hit, bool)> {
    let n = usize::from(slot.len);
    let typed = find_in_row(
        cells,
        &slot.chars[..n],
        slot.caret.1,
        slot.back_chars,
        exact_only,
    );
    if !slot.rtl {
        return typed.map(|hit| (hit, false));
    }
    let mut mirror = slot.chars;
    mirror[..n].reverse();
    let mirrored = find_in_row(
        cells,
        &mirror[..n],
        slot.caret.1,
        slot.back_chars,
        exact_only,
    );
    match (typed, mirrored) {
        (Some(a), Some(b)) if b.exact && !a.exact => Some((b, true)),
        (Some(a), _) => Some((a, false)),
        (None, Some(b)) => Some((b, true)),
        (None, None) => None,
    }
}

/// `row − (base_y_now − base_y_then)`: where a live row stands after the
/// scroll the snapshot reports. With either side unknown the delta is ZERO —
/// the host declared nothing, and the scroll-region fallback covers it.
fn scrolled(row: u16, then: Option<i64>, now: Option<i64>) -> (i64, i64) {
    let delta = match (then, now) {
        (Some(then), Some(now)) => now.saturating_sub(then),
        _ => 0,
    };
    (i64::from(row).saturating_sub(delta), delta)
}

/// Find a pending slot's word. ONLY the expected row is tested; the bounded
/// nearest-first upward search runs when — and only when — the snapshot
/// reported no scroll, the expected row came up empty, and the live `cursor`
/// says the line was LEFT BEHIND (a scroll REGION moved it: tmux, the alt
/// screen), and it takes exact anchors only.
///
/// NOT YET IS NOT GONE. The expected row also comes up empty while the echo is
/// merely late — `$ si` of `sit` — and a search started then can only find
/// something that is not the word: last time's command line, or the `sit` a
/// program printed, wherever a lagging caret happens to line up with it. An
/// echo only ever carries the cursor RIGHTWARD along the caret's row, so a
/// cursor still there, at or past the recorded caret, is a line still being
/// typed; off that row, or left of the caret (a fresh prompt), it is not. A
/// new prompt that grew past the old caret reads as "still typing" and costs
/// that one flash — the safe direction.
#[allow(
    clippy::too_many_arguments,
    reason = "on_rescan's list minus the gate it already applied; the slot, the clock, both carets, the geometry and the row source are each read once"
)]
fn locate(
    slot: &Slot,
    now: Instant,
    cursor: (u16, u16),
    base_y_now: Option<i64>,
    rows: usize,
    grid_cols: u16,
    row_buf: &mut Vec<RenderCell>,
    load_row: &mut impl FnMut(usize, &mut Vec<RenderCell>),
) -> Option<Located> {
    let (expected, delta) = scrolled(slot.caret.0, slot.base_y, base_y_now);
    let mut found = None;
    if let Some(row) = usize::try_from(expected).ok().filter(|row| *row < rows) {
        load_row(row, row_buf);
        found = find_word(slot, row_buf, false).map(|hit| (row, hit));
        let left_behind = cursor.0 != slot.caret.0 || cursor.1 < slot.caret.1;
        if found.is_none() && delta == 0 && left_behind {
            for above in (row.saturating_sub(SEARCH_UP_ROWS)..row).rev() {
                load_row(above, row_buf);
                if let Some(hit) = find_word(slot, row_buf, true) {
                    found = Some((above, hit));
                    break;
                }
            }
        }
    }
    let (row, (hit, mirrored)) = found?;
    // `row_buf` still holds the row the hit came from.
    let mut loc = Located {
        since: now.max(slot.at),
        row: u16::try_from(row).ok()?,
        base_y: base_y_now,
        grid_cols,
        mirrored,
        cols: hit.cols,
        fg: [[0; 3]; MAX_CHARS],
        sat: 0.0,
        val: 0.0,
        strength: 0.0,
    };
    // An unpaintable capture leaves `strength` at zero; the caller drops it.
    let _ = capture(&mut loc, usize::from(slot.len), row_buf);
    Some(loc)
}

/// Re-verify a located flash on a later rescan and follow it by the reported
/// scroll. `None` ends the flash: the grid changed width, the row scrolled
/// off, or the text is no longer there. There is deliberately NO second
/// search — the previous `$ sit` two rows up has the same characters at the
/// same columns, and a flash that jumped to it would be painting history.
fn follow(
    slot: &Slot,
    mut loc: Located,
    base_y_now: Option<i64>,
    rows: usize,
    grid_cols: u16,
    row_buf: &mut Vec<RenderCell>,
    load_row: &mut impl FnMut(usize, &mut Vec<RenderCell>),
) -> Option<Located> {
    if loc.grid_cols != grid_cols {
        return None;
    }
    let (row, _) = scrolled(loc.row, loc.base_y, base_y_now);
    let row = usize::try_from(row).ok().filter(|row| *row < rows)?;
    load_row(row, row_buf);
    let n = usize::from(slot.len);
    let still_there = (0..n).all(|i| {
        let want = slot.chars[if loc.mirrored { n - 1 - i } else { i }];
        row_buf
            .get(usize::from(loc.cols[i]))
            .is_some_and(|cell| !cell.wide && cell.ch == want)
    });
    if !still_there {
        return None;
    }
    loc.row = u16::try_from(row).ok()?;
    loc.base_y = base_y_now;
    // The shell may have recoloured the word since (a syntax highlighter
    // painting an unknown command red): the fade must land on TODAY's colour.
    capture(&mut loc, n, row_buf).then_some(loc)
}

/// Capture what a frame needs from the located row: each lead cell's resolved
/// fg (the ink capture's own source), and — from the first lead cell, the ink
/// guard's convention — the background the legibility guard judges against.
/// `false` when no tint at all would be legible here: there is nothing to
/// show, so there is no flash (see [`legible_strength`]).
fn capture(loc: &mut Located, n: usize, cells: &[RenderCell]) -> bool {
    for i in 0..n {
        if let Some(cell) = cells.get(usize::from(loc.cols[i])) {
            loc.fg[i] = cell.fg;
        }
    }
    let Some(first) = cells.get(usize::from(loc.cols[0])) else {
        return false;
    };
    let bg = rgb3_to_u32(first.bg);
    (loc.sat, loc.val) = resolve_sat_val(bg);
    loc.strength = legible_strength(rgb3_to_u32(first.fg), bg, loc.sat, loc.val);
    loc.strength > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::word_decorations::{DecoConfig, EffectGeom, WordDecorations};
    use aterm_core::render::RenderInput;
    use aterm_core::terminal::Terminal;
    use aterm_lexicon::Lexicon;

    /// Which rescan twin a scenario drives: the cold term walk the batteries
    /// use, or the snapshot path the shipping renderer uses.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Path {
        Cold,
        Snapshot,
    }
    const BOTH: [Path; 2] = [Path::Cold, Path::Snapshot];

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// One terminal and one word engine, driven the way the host drives them:
    /// every echoed key is a rescan, every presented frame is a tick.
    struct Glass {
        term: Terminal,
        wd: WordDecorations,
        lex: Lexicon,
        cfg: DecoConfig,
        rows: usize,
        cols: usize,
        path: Path,
        /// The host declares the snapshot's `base_y` before each rescan.
        wired: bool,
        epoch: u64,
        snap: RenderInput,
    }

    impl Glass {
        fn new(path: Path, rows: usize, cols: usize) -> Self {
            Glass {
                term: Terminal::new(rows as u16, cols as u16),
                wd: WordDecorations::default(),
                lex: Lexicon::with_languages(&["en"]),
                cfg: DecoConfig::default(),
                rows,
                cols,
                path,
                wired: true,
                epoch: 0,
                snap: RenderInput::default(),
            }
        }

        /// A host that never calls `set_scan_base_y`.
        fn unwired(mut self) -> Self {
            self.wired = false;
            self
        }

        fn feed(&mut self, text: &str) {
            self.term.process(text.as_bytes());
        }

        fn caret(&self) -> (u16, u16) {
            let cursor = self.term.cursor();
            (cursor.row, cursor.col)
        }

        /// The rescan an echoed key (or any damage) causes, at `now`.
        fn scan(&mut self, now: Instant) {
            self.scan_with_cursor(now, true);
        }

        /// `with_cursor = false` is the snapshot host's scrollback view.
        fn scan_with_cursor(&mut self, now: Instant, with_cursor: bool) {
            self.epoch += 1;
            match self.path {
                Path::Cold => {
                    if self.wired {
                        let base_y = self.term.grid().base_y() as i64;
                        self.wd.set_scan_base_y(Some(base_y));
                    }
                    self.wd.rescan(
                        &self.term, self.rows, self.cols, &self.lex, &self.cfg, self.epoch, now,
                    );
                }
                Path::Snapshot => {
                    self.term
                        .cell_frame_into(&mut self.snap, self.rows, self.cols);
                    if self.wired {
                        self.wd.set_scan_base_y(Some(self.snap.base_y));
                    }
                    let cursor = with_cursor.then(|| self.caret());
                    self.wd.rescan_from_cells_with_geom_at_cursor(
                        &self.snap.cells,
                        &self.snap.line_sizes,
                        self.rows,
                        self.cols,
                        &self.lex,
                        &self.cfg,
                        self.epoch,
                        now,
                        EffectGeom::default(),
                        self.snap.default_bg,
                        cursor,
                    );
                }
            }
        }

        /// Text the shell echoes and the engine scans, all at `now`.
        fn echo(&mut self, text: &str, now: Instant) {
            self.feed(text);
            self.scan(now);
        }

        /// THE HOST'S FIRE MAPPING: the listener fired on `word` at a boundary
        /// key, so the note lands BEFORE that key's echo is ever scanned.
        fn fire(&mut self, word: &str, back_chars: u16, boundary: &str, now: Instant) {
            self.wd.note_trick_typed(now, None, word, back_chars);
            self.echo(boundary, now);
        }

        /// One presented frame: `(ink, fp)`.
        fn frame(&mut self, now: Instant) -> (Vec<InkCell>, u64) {
            let (mut out, mut ink, mut free, mut nova) = (vec![], vec![], vec![], vec![]);
            let fp = self.wd.tick(
                now,
                &self.cfg,
                EffectGeom::default(),
                None,
                None,
                true,
                &mut out,
                &mut ink,
                &mut free,
                &mut nova,
            );
            (ink, fp)
        }

        fn row(&self, row: usize) -> Vec<RenderCell> {
            let mut cells = Vec::new();
            self.term.render_row_into(row, &mut cells);
            cells
        }
    }

    fn cells_at(ink: &[InkCell]) -> Vec<(u16, u16)> {
        ink.iter().map(|cell| (cell.row, cell.col)).collect()
    }

    fn sorted_unique(ink: &[InkCell]) -> bool {
        ink.windows(2)
            .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col))
    }

    /// What the law says lead cell `i` of an `n`-cell word typed at `caret`
    /// shows at `t` ms over `base`, judged against the word's first-cell
    /// colours — the emitter's own private pieces, composed by hand.
    fn law(
        caret: (u16, u16),
        word: &str,
        i: usize,
        t: u64,
        base: [u8; 3],
        first_fg: [u8; 3],
        bg: [u8; 3],
    ) -> [u8; 3] {
        let chars: Vec<char> = word.chars().collect();
        let (sat, val) = resolve_sat_val(rgb3_to_u32(bg));
        let strength = legible_strength(rgb3_to_u32(first_fg), rgb3_to_u32(bg), sat, val);
        let hue0 = rainbow_base_hue(hue_seed(caret, &chars));
        let target = hsv2rgb(flash_hue(hue0, i, chars.len(), t), sat, val);
        u32_to_rgb3(mix_rgb(
            rgb3_to_u32(base),
            target,
            envelope(t, None) * strength,
        ))
    }

    /// THE HEADLINE, on both rescan twins: a word typed to the pet flashes on
    /// its own cells, every animating frame fingerprints differently, and past
    /// the window there are ZERO cells, a fingerprint of 0 that stays 0, and a
    /// scheduler nothing is holding awake.
    #[test]
    fn a_typed_word_flashes_then_emits_nothing_with_a_stable_fp_and_a_disarmed_scheduler() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo("$ sit", t0);
            assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Idle);
            g.wd.note_trick_typed(t0, None, "sit", 0);
            assert_eq!(
                g.wd.trick_flash_phase(t0),
                TrickFlashPhase::Pending,
                "{path:?}: noted, not yet found"
            );
            let (ink, fp) = g.frame(t0);
            assert!(
                ink.is_empty() && fp == 0,
                "{path:?}: a pending flash paints nothing"
            );
            g.echo(" ", t0);
            assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Live);

            let (ink, fp_a) = g.frame(t0 + ms(300));
            assert_eq!(
                cells_at(&ink),
                [(0, 2), (0, 3), (0, 4)],
                "{path:?}: one cell per glyph of `sit`"
            );
            assert_ne!(fp_a, 0);
            assert!(
                g.wd.is_active(t0 + ms(300)),
                "{path:?}: a live flash on a wordless screen must arm the scheduler"
            );
            let (_, fp_b) = g.frame(t0 + ms(316));
            assert_ne!(
                fp_a, fp_b,
                "{path:?}: an animating flash never repeats an fp"
            );

            let done = t0 + ms(u64::from(FLASH_MS));
            let (ink, fp) = g.frame(done);
            assert!(ink.is_empty(), "{path:?}: past the window, zero cells");
            assert_eq!(fp, 0, "{path:?}: …and the pre-feature fingerprint");
            assert_eq!(g.frame(done + ms(16)).1, 0, "{path:?}: …which is stable");
            assert!(!g.wd.is_active(done), "{path:?}: …and nothing is armed");
            assert_eq!(g.wd.trick_flash_phase(done), TrickFlashPhase::Idle);
            // A later repaint of the same text is just text: the slot is gone.
            g.scan(done + ms(32));
            assert!(g.frame(done + ms(48)).0.is_empty());
        }
    }

    /// The two rescan twins are ONE routine outside row sourcing: the same
    /// scenario paints the same bytes and folds the same fingerprints.
    #[test]
    fn the_cold_path_and_the_snapshot_path_paint_the_same_bytes() {
        let t0 = Instant::now();
        let mut runs = BOTH.map(|path| {
            let mut g = Glass::new(path, 4, 40);
            g.echo("$ good kitty", t0);
            g.fire("good", 6, " ", t0);
            [0u64, 90, 400, 1000, 1200].map(|t| g.frame(t0 + ms(t)))
        });
        let snapshot = runs[1].clone();
        assert_eq!(runs[0], snapshot);
        assert!(runs[0].iter_mut().any(|(ink, _)| !ink.is_empty()));
    }

    /// The envelope's shape: zero at both ends (the no-residue law), a smooth
    /// monotone ramp, a flat hold at exactly 1, a smooth monotone fade.
    #[test]
    fn the_envelope_ramps_in_holds_and_fades_to_zero_at_both_ends() {
        let (ramp, fade_from, whole) = (
            u64::from(RAMP_MS),
            u64::from(FLASH_MS - FADE_MS),
            u64::from(FLASH_MS),
        );
        assert_eq!(envelope(0, None), 0.0);
        assert_eq!(envelope(whole, None), 0.0);
        for t in 1..=ramp {
            assert!(
                envelope(t, None) > envelope(t - 1, None),
                "ramp must rise at {t} ms"
            );
        }
        for t in ramp..=fade_from {
            assert_eq!(envelope(t, None), 1.0, "hold at {t} ms");
        }
        for t in fade_from + 1..=whole {
            assert!(
                envelope(t, None) < envelope(t - 1, None),
                "fade must fall at {t} ms"
            );
        }
        // Smooth means no step anywhere: the biggest 1 ms move is small.
        let biggest = (1..=whole)
            .map(|t| (envelope(t, None) - envelope(t - 1, None)).abs())
            .fold(0.0f32, f32::max);
        assert!(biggest < 0.02, "a {biggest} jump in one millisecond");
    }

    /// The painted bytes ARE the law, cell for cell, at every phase of the
    /// flash: `mix(base, hsv(seed + u·span + 360·t/FLASH), env(t)·strength)`
    /// with the word's own captured colours as the base.
    #[test]
    fn the_painted_bytes_are_the_envelope_law_cell_for_cell() {
        let t0 = Instant::now();
        let mut g = Glass::new(Path::Cold, 4, 40);
        // A coloured word on a coloured cell: the base is the CELL's colour.
        g.echo("$ \x1b[38;2;230;200;40m\x1b[48;2;20;24;40mplay", t0);
        let caret = g.caret();
        g.fire("play", 0, " ", t0);
        let row = g.row(0);
        let (first_fg, bg) = (row[2].fg, row[2].bg);
        assert_eq!((first_fg, bg), ([230, 200, 40], [20, 24, 40]));
        for t in [0u64, 40, 129, 130, 600, 849, 850, 1000, 1099] {
            let (ink, _) = g.frame(t0 + ms(t));
            assert_eq!(ink.len(), 4, "t = {t}");
            for (i, cell) in ink.iter().enumerate() {
                assert_eq!(
                    cell.color,
                    law(caret, "play", i, t, row[2 + i].fg, first_fg, bg),
                    "t = {t}, cell {i}"
                );
            }
        }
        // …and the flash visibly IS a rainbow mid-hold: the cells differ from
        // the base and from each other.
        let (ink, _) = g.frame(t0 + ms(500));
        assert!(ink.iter().all(|cell| cell.color != first_fg));
        assert_ne!(ink[0].color, ink[3].color);
    }

    /// NO RESIDUE: the first frame is exactly the word's own colour, the last
    /// frames converge on it to within one LSB, and then there are no cells.
    #[test]
    fn the_fade_lands_back_on_the_words_own_colour() {
        let t0 = Instant::now();
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("$ \x1b[38;2;200;80;40mjump", t0);
        g.fire("jump", 0, " ", t0);
        let own = [200u8, 80, 40];
        let (ink, _) = g.frame(t0);
        assert!(
            ink.iter().all(|cell| cell.color == own),
            "t = 0 is the base"
        );
        let (ink, _) = g.frame(t0 + ms(u64::from(FLASH_MS) - 1));
        assert_eq!(ink.len(), 4);
        for cell in &ink {
            for (got, want) in cell.color.iter().zip(own) {
                assert!(
                    got.abs_diff(want) <= 1,
                    "the last frame is {:?}, the word is {own:?}",
                    cell.color
                );
            }
        }
        assert!(g.frame(t0 + ms(u64::from(FLASH_MS))).0.is_empty());
    }

    /// The base is TODAY's colour. A syntax highlighter repaints the word a
    /// beat after the flash began (an unknown command going red): every later
    /// rescan recaptures, so the frames mix from the new colour and the fade
    /// lands on it — not on the colour the word had when it was found.
    #[test]
    fn a_word_recoloured_mid_flash_fades_to_the_colour_it_has_now() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo("$ sit", t0);
            let caret = g.caret();
            g.fire("sit", 0, " ", t0);
            let was = g.row(0)[2].fg;
            let red = [255u8, 85, 85];
            assert_ne!(was, red);
            g.echo("\r$ \x1b[38;2;255;85;85msit\x1b[m ", t0 + ms(100));
            let bg = g.row(0)[2].bg;
            let (ink, _) = g.frame(t0 + ms(500));
            assert_eq!(ink.len(), 3, "{path:?}: the same word, still flashing");
            for (i, cell) in ink.iter().enumerate() {
                assert_eq!(
                    cell.color,
                    law(caret, "sit", i, 500, red, red, bg),
                    "{path:?}: cell {i} mixes from the recaptured colour"
                );
            }
            let (ink, _) = g.frame(t0 + ms(u64::from(FLASH_MS) - 1));
            for cell in &ink {
                for (got, want) in cell.color.iter().zip(red) {
                    assert!(
                        got.abs_diff(want) <= 1,
                        "{path:?}: the last frame is {:?}, the word is now {red:?}",
                        cell.color
                    );
                }
            }
        }
    }

    /// TYPED, NEVER SEEN: `echo sit`, its output, and even a committed edit
    /// keystroke beside it mint nothing. Only the input path's note does.
    #[test]
    fn echo_sit_output_never_inks_without_a_note() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 6, 40);
            g.echo("$ echo sit\r\nsit\r\n$ sit ", t0);
            g.wd.note_typed_edit(t0, None);
            g.scan(t0);
            for t in [0u64, 16, 300, 1100, 2000] {
                let (ink, fp) = g.frame(t0 + ms(t));
                assert!(ink.is_empty() && fp == 0, "{path:?}: t = {t}");
                assert_eq!(g.wd.trick_flash_phase(t0 + ms(t)), TrickFlashPhase::Idle);
            }
            assert!(!g.wd.is_active(t0));
        }
    }

    /// THE ROW LAW. `sit⏎` on the bottom row scrolls before its echo is ever
    /// scanned, and the prompt width here is the ADVERSARIAL one: the shell's
    /// `command not found: sit` puts its `sit` in exactly the typed word's
    /// columns, one row nearer the caret. With the snapshot's `base_y` declared
    /// the engine tests one row — the typed line's — and never the shell's.
    #[test]
    fn enter_on_the_bottom_row_is_found_on_the_scrolled_row_never_on_the_shells_error_line() {
        let prompt = format!("{}$ ", "~".repeat(22));
        let script = |g: &mut Glass, t0: Instant| {
            g.echo(&format!("one\r\ntwo\r\nthree\r\n{prompt}sit"), t0);
            assert_eq!(g.caret(), (3, 27), "typed on the bottom row");
            // Enter is the boundary key: noted first, then the shell answers.
            g.fire(
                "sit",
                0,
                &format!("\r\nzsh: command not found: sit\r\n{prompt}"),
                t0,
            );
            let typed: String = g.row(1).iter().map(|cell| cell.ch).collect();
            let error: String = g.row(2).iter().map(|cell| cell.ch).collect();
            assert!(typed.starts_with(&format!("{prompt}sit")), "{typed:?}");
            assert_eq!(&error[24..27], "sit", "the adversarial column: {error:?}");
            g.frame(t0 + ms(300)).0
        };
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            assert_eq!(
                cells_at(&script(&mut g, t0)),
                [(1, 24), (1, 25), (1, 26)],
                "{path:?}: the TYPED line, two rows up"
            );
            // NEGATIVE CONTROL — why the host declares `base_y` at all. With no
            // delta to go on, the nearest exactly-anchored match IS the shell's
            // own output at this prompt width. This is the residual the setter
            // closes; a host that wires it never reaches this search.
            let mut blind = Glass::new(path, 4, 40).unwired();
            assert_eq!(
                cells_at(&script(&mut blind, t0)),
                [(2, 24), (2, 25), (2, 26)],
                "{path:?}: non-vacuous — without base_y the error line wins"
            );
        }
    }

    /// At any OTHER prompt width the undeclared fallback is sound too: the
    /// error line's `sit` is not exactly anchored, so nearest-first walks past
    /// it to the typed line.
    #[test]
    fn without_base_y_the_upward_search_skips_a_match_that_is_not_exactly_anchored() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40).unwired();
            g.echo("one\r\ntwo\r\nthree\r\n$ sit", t0);
            g.fire("sit", 0, "\r\nzsh: command not found: sit\r\n$ ", t0);
            assert_eq!(
                cells_at(&g.frame(t0 + ms(300)).0),
                [(1, 2), (1, 3), (1, 4)],
                "{path:?}"
            );
        }
    }

    /// A scroll REGION (tmux, a pager's status line) moves the typed line
    /// without moving `base_y`: the delta is zero, the caret's row comes up
    /// empty, and the bounded upward search takes over — EXACT anchors only.
    #[test]
    fn a_scroll_region_scroll_is_followed_upward_by_exact_anchor_only() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 6, 40);
            // Rows 1..=4 scroll; row 0 and row 5 are chrome. Type on row 4.
            g.echo("\x1b[2;5r\x1b[5;1H$ sit", t0);
            assert_eq!(g.caret(), (4, 5));
            let base_y = g.term.grid().base_y();
            g.fire("sit", 0, "\r\nzsh: command not found: sit\r\n$ ", t0);
            assert_eq!(g.term.grid().base_y(), base_y, "a region scroll");
            assert_eq!(
                cells_at(&g.frame(t0 + ms(300)).0),
                [(2, 2), (2, 3), (2, 4)],
                "{path:?}: past the error line (not exactly anchored) to the typed one"
            );

            // The other half: a caret recorded MID-WORD (a burst that outran
            // its echo) is a tolerant anchor. Tolerance is honoured on the
            // expected row and REFUSED by the upward search.
            let mut g = Glass::new(path, 6, 40);
            g.echo("\x1b[2;5r\x1b[5;1H$ s", t0);
            g.wd.note_trick_typed(t0, None, "sit", 0);
            let mut still = Glass::new(path, 6, 40);
            still.echo("\x1b[2;5r\x1b[5;1H$ s", t0);
            still.wd.note_trick_typed(t0, None, "sit", 0);
            still.echo("it ", t0);
            assert_eq!(
                cells_at(&still.frame(t0 + ms(300)).0),
                [(4, 2), (4, 3), (4, 4)],
                "{path:?}: a lagging caret still finds the word on ITS row"
            );
            g.echo("it\r\nzsh: command not found: sit\r\n$ ", t0);
            assert!(
                g.frame(t0 + ms(300)).0.is_empty(),
                "{path:?}: …but never by searching upward"
            );

            // A HELD fire under the same scroll: `good kitty⏎`. EXACT is the
            // caret's distance in TYPED CHARACTERS (`back_chars`), not plain
            // adjacency — or a held word could never be followed upward.
            let mut g = Glass::new(path, 6, 40);
            g.echo("\x1b[2;5r\x1b[5;1H$ good kitty", t0);
            g.fire("good", 6, "\r\nzsh: command not found: good\r\n$ ", t0);
            let flashed: Vec<(u16, u16)> = cells_at(&g.frame(t0 + ms(300)).0)
                .into_iter()
                .filter(|(_, col)| *col < 6)
                .collect();
            assert_eq!(flashed, [(2, 2), (2, 3), (2, 4), (2, 5)], "{path:?}");

            // LEFT BEHIND is a fact about the live cursor, and a cursor on
            // ANOTHER ROW is as good as one left of the caret. Typed one row
            // above the region's floor, the failing command leaves a prompt
            // that GREW (`[127] $ `) on the floor itself: right of the old
            // caret's column, but no longer on its row.
            let mut g = Glass::new(path, 6, 40);
            g.echo("\x1b[2;5r\x1b[4;1H$ sit", t0);
            assert_eq!(g.caret(), (3, 5));
            g.fire("sit", 0, "\r\nzsh: command not found: sit\r\n[127] $ ", t0);
            assert_eq!(g.caret(), (4, 8));
            assert_eq!(
                cells_at(&g.frame(t0 + ms(300)).0),
                [(2, 2), (2, 3), (2, 4)],
                "{path:?}: the cursor left the caret's row"
            );
            // KNOWN COST, pinned so whoever lifts it knows which line to
            // change: the same grown prompt landing on the caret's OWN row, at
            // or past its column, is indistinguishable from an echo still
            // arriving, and the search stays home. Silence, the safe direction.
            let mut g = Glass::new(path, 6, 40);
            g.echo("\x1b[2;5r\x1b[5;1H$ sit", t0);
            g.fire("sit", 0, "\r\nzsh: command not found: sit\r\n[127] $ ", t0);
            assert_eq!(g.caret(), (4, 8));
            assert!(g.frame(t0 + ms(300)).0.is_empty(), "{path:?}");
        }
    }

    /// A REPORTED scroll is the whole truth: when the snapshot says the line
    /// moved and the word is not on the row it moved to, the engine does NOT go
    /// looking. Here the typed line was wiped as it scrolled, and last time's
    /// `$ sit` stands exactly anchored a few rows up — history, not the word.
    #[test]
    fn a_reported_scroll_is_never_second_guessed_by_searching() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 6, 40);
            g.echo(
                "a\r\nb\r\n$ sit\r\nzsh: command not found: sit\r\nc\r\n$ sit",
                t0,
            );
            assert_eq!(g.caret(), (5, 5));
            g.fire("sit", 0, "\r\x1b[K\r\n$ ", t0);
            let history: String = g.row(1).iter().take(5).map(|cell| cell.ch).collect();
            assert_eq!(history, "$ sit", "exactly anchored, three rows above");
            assert!(g.frame(t0 + ms(300)).0.is_empty(), "{path:?}");
            // NEGATIVE CONTROL: the undeclared host's fallback DOES find it,
            // which is what makes the silence above a decision, not an accident.
            let mut blind = Glass::new(path, 6, 40).unwired();
            blind.echo(
                "a\r\nb\r\n$ sit\r\nzsh: command not found: sit\r\nc\r\n$ sit",
                t0,
            );
            blind.fire("sit", 0, "\r\x1b[K\r\n$ ", t0);
            assert_eq!(
                cells_at(&blind.frame(t0 + ms(300)).0)[0],
                (1, 2),
                "{path:?}"
            );
        }
    }

    /// NOT YET IS NOT GONE. A burst (`aterm ctl key`, a slow link) commits
    /// `sit␣` before any of it has echoed, so the caret on record is still the
    /// prompt's end and the first rescan after the note sees `$ s`. The caret's
    /// row coming up empty THERE means the echo is late, not that the line
    /// moved — and last time's `$ sit`, two rows up, STARTS at exactly that
    /// caret. The upward search must not take it, declared `base_y` or not: it
    /// follows a word the engine had already seen in full, never one it is
    /// still waiting for.
    #[test]
    fn a_late_echo_never_sends_the_search_to_last_times_line() {
        for path in BOTH {
            for wired in [true, false] {
                let t0 = Instant::now();
                let mut g = Glass::new(path, 8, 40);
                g.wired = wired;
                g.echo("$ sit\r\nzsh: command not found: sit\r\n$ ", t0);
                assert_eq!(g.caret(), (2, 2), "the prompt's end, nothing typed yet");
                g.wd.note_trick_typed(t0, None, "sit", 0);
                g.echo("s", t0 + ms(40));
                assert_eq!(
                    g.wd.trick_flash_phase(t0 + ms(40)),
                    TrickFlashPhase::Pending,
                    "{path:?}, wired = {wired}: `$ s` is a word still arriving"
                );
                assert!(
                    g.frame(t0 + ms(50)).0.is_empty(),
                    "{path:?}, wired = {wired}: history never flashes"
                );
                // A program parks the cursor elsewhere while the echo is still
                // in flight (a status line drawn without a restore): the cursor
                // HAS left the caret's row, so the search may run — and it
                // still refuses the reading that says the word was never seen.
                g.echo("\x1b[1;1H", t0 + ms(60));
                assert_eq!(g.caret(), (0, 0));
                assert_eq!(
                    g.wd.trick_flash_phase(t0 + ms(60)),
                    TrickFlashPhase::Pending,
                    "{path:?}, wired = {wired}: `$ sit` two rows up STARTS at the caret, and is history"
                );
                g.echo("\x1b[3;4Hit ", t0 + ms(80));
                assert_eq!(
                    cells_at(&g.frame(t0 + ms(300)).0),
                    [(2, 2), (2, 3), (2, 4)],
                    "{path:?}, wired = {wired}: the word itself, once it lands"
                );

                // The same lateness one letter further on, and the LEFT reading
                // lines up by coincidence instead: with `s` echoed the caret
                // on record is column 3, and the `sit` that `echo sit` PRINTED
                // ends at exactly column 2. What rules it out is the live
                // cursor: it has only moved RIGHT of where the key was
                // committed, so the line is still being echoed, not left behind.
                let mut g = Glass::new(path, 8, 40);
                g.wired = wired;
                g.echo("$ echo sit\r\nsit\r\n$ s", t0);
                assert_eq!(g.caret(), (2, 3));
                g.wd.note_trick_typed(t0, None, "sit", 0);
                // A repaint elsewhere (a status clock, cursor saved and
                // restored) rescans before ANY more echo: the cursor has not
                // moved at all, which is still not "left behind".
                g.echo("\x1b7\x1b[1;30H12:00\x1b8", t0 + ms(20));
                assert_eq!(g.caret(), (2, 3));
                assert_eq!(
                    g.wd.trick_flash_phase(t0 + ms(20)),
                    TrickFlashPhase::Pending,
                    "{path:?}, wired = {wired}: nothing has moved"
                );
                g.echo("i", t0 + ms(40));
                assert_eq!(
                    g.wd.trick_flash_phase(t0 + ms(40)),
                    TrickFlashPhase::Pending,
                    "{path:?}, wired = {wired}: `$ si` is a word still arriving"
                );
                assert!(
                    g.frame(t0 + ms(50)).0.is_empty(),
                    "{path:?}, wired = {wired}: program output never flashes"
                );
                g.echo("t ", t0 + ms(80));
                assert_eq!(
                    cells_at(&g.frame(t0 + ms(300)).0),
                    [(2, 2), (2, 3), (2, 4)],
                    "{path:?}, wired = {wired}"
                );
            }
        }
    }

    /// A DECLARATION IS SPENT BY THE RESCAN IT WAS MADE FOR. A rescan the host
    /// did not declare for (an entry point it has not wired) must read as "no
    /// row origin", never as the last declared one: the caret it records would
    /// otherwise be paired with a `base_y` from before the scroll, the next
    /// declared rescan would report a scroll that never happened to THIS caret,
    /// and the one row tested would be last time's `$ sit`.
    #[test]
    fn a_declaration_is_spent_by_the_rescan_it_was_made_for() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 6, 40);
            g.echo("a\r\nb\r\nc\r\nd\r\ne\r\n$ sit", t0);
            // Two rows scroll by under a rescan nobody declared for.
            g.wired = false;
            g.echo(" \r\nzsh: command not found: sit\r\n$ sit", t0);
            assert_eq!(g.caret(), (5, 5));
            let history: String = g.row(3).iter().take(5).map(|cell| cell.ch).collect();
            assert_eq!(history, "$ sit", "exactly anchored, two rows above");
            g.wired = true;
            g.fire("sit", 0, " ", t0 + ms(2000));
            assert_eq!(
                cells_at(&g.frame(t0 + ms(2300)).0),
                [(5, 2), (5, 3), (5, 4)],
                "{path:?}: the typed line, not the one a stale origin points at"
            );
        }
    }

    /// The preference order on the caret's row, pinned on the routine itself:
    /// an EXACT anchor beats a tolerant one wherever it stands, among equals
    /// the RIGHTMOST wins (the word just typed is the last thing on its line),
    /// and out of reach is no hit at all.
    #[test]
    fn an_exact_anchor_beats_a_tolerant_one_and_among_equals_the_rightmost_wins() {
        let mut g = Glass::new(Path::Cold, 2, 40);
        g.feed("$ sit sit");
        let row = g.row(0);
        let find = |caret_col: u16| {
            find_in_row(&row, &['s', 'i', 't'], caret_col, 0, false)
                .map(|hit| (hit.cols[0], hit.exact))
        };
        assert_eq!(
            find(5),
            Some((2, true)),
            "exact on the left, though the right one is within reach"
        );
        assert_eq!(find(8), Some((6, false)), "both tolerant: the rightmost");
        assert_eq!(find(9), Some((6, true)));
        assert_eq!(find(6), Some((6, true)), "the caret at the word's START");
        assert_eq!(find(20), None, "out of reach");
    }

    /// A HELD address word sits a few typed characters LEFT of the caret:
    /// `good kitty␣` fires at the pet's name and flashes `good`, found through
    /// `back_chars`. Without it the same note is out of reach — and so unpainted.
    #[test]
    fn a_held_address_word_is_found_behind_the_pets_name_by_back_chars() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo("$ good kitty", t0);
            g.fire("good", 6, " ", t0);
            let (ink, _) = g.frame(t0 + ms(500));
            let flashed: Vec<(u16, u16)> = cells_at(&ink)
                .into_iter()
                .filter(|(_, col)| *col < 6)
                .collect();
            assert_eq!(flashed, [(0, 2), (0, 3), (0, 4), (0, 5)], "{path:?}");
            assert!(sorted_unique(&ink));

            let mut adjacent_only = Glass::new(path, 4, 40);
            adjacent_only.echo("$ good kitty", t0);
            adjacent_only.fire("good", 0, " ", t0);
            let (ink, _) = adjacent_only.frame(t0 + ms(500));
            assert!(
                cells_at(&ink).iter().all(|(_, col)| *col >= 7),
                "{path:?}: non-vacuous — back_chars is what reaches `good`"
            );
        }
    }

    /// The split host ticks ONE engine once per pane and translates each
    /// pane's ink by that pane's origin. Pane B holds the very same text at the
    /// very same cells, so an ungated flash would paint it too.
    #[test]
    fn pane_b_never_shows_pane_as_flash() {
        let t0 = Instant::now();
        let (lex, cfg) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
        let mut a = Terminal::new(4, 40);
        let mut b = Terminal::new(4, 40);
        a.process(b"$ sit");
        b.process(b"$ sit");
        // A third pane holds the word two columns along — still within reach
        // of pane A's caret, so an ungated RESCAN would find pane A's word on
        // pane C's glass and pane A's flash would die at its own next rescan.
        let mut c = Terminal::new(4, 40);
        c.process(b"$$$ sit ");
        let mut wd = WordDecorations::default();
        let tick = |wd: &mut WordDecorations, now: Instant| {
            let (mut out, mut ink, mut free, mut nova) = (vec![], vec![], vec![], vec![]);
            let geom = EffectGeom::default();
            let fp = wd.tick(
                now, &cfg, geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
            );
            (ink, fp)
        };
        wd.bind_pane(1, (0, 0));
        wd.rescan(&a, 4, 40, &lex, &cfg, 1, t0);
        wd.bind_pane(2, (400, 0));
        wd.rescan(&b, 4, 40, &lex, &cfg, 1, t0);
        // The key goes to pane 1 while pane 2 is the one left bound: the caret
        // must come from pane 1's PARKED slot.
        wd.note_trick_typed(t0, Some(1), "sit", 0);
        a.process(b" ");
        b.process(b" ");
        // The composed host scans the panes in layout order, not typing order.
        wd.bind_pane(3, (800, 0));
        wd.rescan(&c, 4, 40, &lex, &cfg, 1, t0);
        assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
        assert!(tick(&mut wd, t0).0.is_empty());
        wd.bind_pane(1, (0, 0));
        wd.rescan(&a, 4, 40, &lex, &cfg, 2, t0);
        let (ink_a, _) = tick(&mut wd, t0 + ms(300));
        assert_eq!(cells_at(&ink_a), [(0, 2), (0, 3), (0, 4)]);
        wd.bind_pane(2, (400, 0));
        wd.rescan(&b, 4, 40, &lex, &cfg, 2, t0);
        let (ink_b, fp_b) = tick(&mut wd, t0 + ms(300));
        assert!(
            ink_b.is_empty() && fp_b == 0,
            "pane B's tick is the pre-feature path"
        );
        assert_eq!(
            wd.trick_flash_phase(t0 + ms(300)),
            TrickFlashPhase::Live,
            "pane B's rescan must not collect pane A's flash either"
        );
        wd.bind_pane(1, (0, 0));
        assert_eq!(tick(&mut wd, t0 + ms(316)).0.len(), 3);
        // The wake rides pane A's OWN parked deadline and dies with it.
        wd.bind_pane(2, (400, 0));
        assert!(wd.is_active(t0 + ms(316)));
        assert!(!wd.is_active(t0 + ms(u64::from(FLASH_MS))));
    }

    /// The flash is not parked, so nothing prunes it with the parked panes: the
    /// ENGINE's `retain_panes` has to reach it. A flash naming a pane that left
    /// the layout goes with the pane — no rescan of it will ever come to
    /// collect it — and one naming a pane that stayed is untouched.
    #[test]
    fn closing_the_pane_takes_its_flash_with_it() {
        let t0 = Instant::now();
        let (lex, cfg) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
        for (closed, left) in [(1u64, TrickFlashPhase::Idle), (2, TrickFlashPhase::Live)] {
            let mut a = Terminal::new(4, 40);
            a.process(b"$ sit");
            let mut wd = WordDecorations::default();
            wd.bind_pane(1, (0, 0));
            wd.rescan(&a, 4, 40, &lex, &cfg, 1, t0);
            wd.note_trick_typed(t0, Some(1), "sit", 0);
            a.process(b" ");
            wd.rescan(&a, 4, 40, &lex, &cfg, 2, t0);
            wd.bind_pane(2, (400, 0));
            assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Live);
            wd.retain_panes(|pane| pane != closed);
            assert_eq!(wd.trick_flash_phase(t0), left, "pane {closed} closed");
        }
    }

    /// A host that names NO pane in the note but declares its scan session:
    /// the row origin was recorded under that session, and the note must read
    /// it from there — or the adversarial prompt width is back.
    #[test]
    fn a_note_that_names_no_pane_reads_the_scan_sessions_row_origin() {
        let prompt = format!("{}$ ", "~".repeat(22));
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.wd.set_scan_session(Some(10));
            g.echo(&format!("one\r\ntwo\r\nthree\r\n{prompt}sit"), t0);
            g.fire(
                "sit",
                0,
                &format!("\r\nzsh: command not found: sit\r\n{prompt}"),
                t0,
            );
            assert_eq!(
                cells_at(&g.frame(t0 + ms(300)).0),
                [(1, 24), (1, 25), (1, 26)],
                "{path:?}: the typed line, not the shell's"
            );
        }
    }

    /// The flash never joins `is_active` as a term of its own (the retired
    /// cameo's bug: a wake that outlives a tab switch with nothing left to
    /// tick it down). Tab B's tick is the pre-feature path, and the only thing
    /// still armed is tab A's own deadline, which the clock alone retires.
    #[test]
    fn a_tab_switch_never_lets_the_flash_hold_the_scheduler_past_its_own_window() {
        let t0 = Instant::now();
        let mut g = Glass::new(Path::Snapshot, 4, 40);
        g.wd.set_scan_session(Some(10));
        g.echo("$ sit", t0);
        g.wd.note_trick_typed(t0, Some(10), "sit", 0);
        g.echo(" ", t0);
        assert_eq!(g.frame(t0 + ms(200)).0.len(), 3);

        // Tab B shows the same text at the same cells.
        g.wd.set_scan_session(Some(20));
        g.scan(t0 + ms(210));
        let (ink, fp) = g.frame(t0 + ms(216));
        assert!(ink.is_empty() && fp == 0, "tab B never shows tab A's flash");
        let end = t0 + ms(u64::from(FLASH_MS));
        assert!(
            !g.wd.is_active(end),
            "with no further tick of tab A, the wake still ends with the flash"
        );
        // …and coming back after the window finds nothing left to paint.
        g.wd.set_scan_session(Some(10));
        g.scan(end + ms(10));
        let (ink, fp) = g.frame(end + ms(16));
        assert!(ink.is_empty() && fp == 0);
        assert!(!g.wd.is_active(end + ms(16)));
    }

    /// A rescan of the flash's pane with NO cursor is a scrollback view: the
    /// viewport's rows are not live rows, so the flash ends — live or pending.
    #[test]
    fn a_rescan_without_a_cursor_ends_the_flash() {
        let t0 = Instant::now();
        // The snapshot host passes `None` while the view is scrolled back.
        for pending in [false, true] {
            let mut g = Glass::new(Path::Snapshot, 4, 40);
            g.echo("$ sit", t0);
            g.wd.note_trick_typed(t0, None, "sit", 0);
            if !pending {
                g.echo(" ", t0);
                assert_eq!(g.frame(t0 + ms(100)).0.len(), 3);
            }
            g.scan_with_cursor(t0 + ms(110), false);
            assert_eq!(g.wd.trick_flash_phase(t0 + ms(110)), TrickFlashPhase::Idle);
            let (ink, fp) = g.frame(t0 + ms(116));
            assert!(ink.is_empty() && fp == 0, "pending = {pending}");
            g.scan(t0 + ms(120));
            assert!(g.frame(t0 + ms(130)).0.is_empty(), "…and it stays ended");
        }
        // The cold path derives the same fact from the display offset.
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("a\r\nb\r\nc\r\nd\r\n$ sit", t0);
        g.fire("sit", 0, " ", t0);
        assert_eq!(g.frame(t0 + ms(100)).0.len(), 3);
        g.term.scroll_display(1);
        assert_ne!(g.term.grid().display_offset(), 0, "scrolled back");
        g.scan(t0 + ms(110));
        assert!(g.frame(t0 + ms(116)).0.is_empty());
        assert_eq!(g.wd.trick_flash_phase(t0 + ms(116)), TrickFlashPhase::Idle);
    }

    /// NEVER PAINT A PLACE YOU HAVE NOT FOUND, and do not wait for ever: an
    /// echo that lands inside the budget is found, one that lands after it is
    /// not — and the phase reads idle the moment the budget lapses.
    #[test]
    fn a_word_not_found_within_the_echo_window_is_given_up_unpainted() {
        for path in BOTH {
            for (late, found) in [(GIVE_UP_MS, true), (GIVE_UP_MS + 1, false)] {
                let t0 = Instant::now();
                let mut g = Glass::new(path, 4, 40);
                g.echo("$ sit", t0);
                g.wd.note_trick_typed(t0, None, "sit", 0);
                let then = t0 + ms(late);
                assert_eq!(
                    g.wd.trick_flash_phase(then) == TrickFlashPhase::Pending,
                    found,
                    "{path:?}: the phase is a pure function of the clock"
                );
                g.echo(" ", then);
                assert_eq!(
                    !g.frame(then + ms(200)).0.is_empty(),
                    found,
                    "{path:?}: echo after {late} ms"
                );
            }
        }
    }

    /// The envelope runs from the RESCAN that found the word, not from the
    /// key: a late echo still ramps in from the base instead of popping.
    #[test]
    fn a_late_echo_still_ramps_in_from_the_base() {
        let t0 = Instant::now();
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("$ sit", t0);
        g.wd.note_trick_typed(t0, None, "sit", 0);
        let echoed = t0 + ms(400);
        g.echo(" ", echoed);
        let own = g.row(0)[2].fg;
        let (ink, _) = g.frame(echoed);
        assert!(ink.iter().all(|cell| cell.color == own) && ink.len() == 3);
        let end = echoed + ms(u64::from(FLASH_MS));
        assert!(!g.frame(end - ms(1)).0.is_empty());
        assert!(g.frame(end).0.is_empty());
        // A REWOUND rescan clock (the capture path) never predates the key.
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("$ sit", t0);
        g.wd.note_trick_typed(t0 + ms(50), None, "sit", 0);
        g.echo(" ", t0);
        assert_eq!(g.frame(t0 + ms(50)).0[0].color, own, "since = the key time");
    }

    /// `sit tight`: the line turned out to be prose. A flash on the glass
    /// leaves over [`REVOKE_FADE_MS`] — smoothly, from where it stood — and one
    /// the rescan never found is dropped unseen. Revoking is idempotent.
    #[test]
    fn a_revoke_fast_fades_a_live_flash_and_drops_an_unfound_one() {
        let t0 = Instant::now();
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.wd.revoke_trick_flash(t0); // nothing pending: a no-op
        g.echo("$ sit", t0);
        g.fire("sit", 0, " ", t0);
        let own = g.row(0)[2].fg;
        let before = g.frame(t0 + ms(400)).0;
        g.wd.revoke_trick_flash(t0 + ms(400));
        assert_eq!(
            g.wd.trick_flash_phase(t0 + ms(400)),
            TrickFlashPhase::Revoked
        );
        assert_eq!(
            g.frame(t0 + ms(400)).0,
            before,
            "continuous at the revoke: no pop"
        );
        let dist = |ink: &[InkCell]| -> u32 {
            ink.iter()
                .flat_map(|cell| cell.color.into_iter().zip(own))
                .map(|(a, b)| u32::from(a.abs_diff(b)))
                .sum()
        };
        let mid = g.frame(t0 + ms(460)).0;
        assert!(
            dist(&mid) < dist(&before) && dist(&mid) > 0,
            "half-way through the fast fade the tint is going, not gone"
        );
        // A second revoke must not restart the fade.
        g.wd.revoke_trick_flash(t0 + ms(500));
        let gone = t0 + ms(400 + u64::from(REVOKE_FADE_MS));
        assert!(!g.frame(gone - ms(1)).0.is_empty());
        let (ink, fp) = g.frame(gone);
        assert!(ink.is_empty() && fp == 0);
        assert!(
            !g.wd.is_active(gone),
            "the deadline came in with the revoke"
        );
        assert_eq!(g.wd.trick_flash_phase(gone), TrickFlashPhase::Idle);

        // Revoked BEFORE the echo: never painted at all.
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("$ sit", t0);
        g.wd.note_trick_typed(t0, None, "sit", 0);
        g.wd.revoke_trick_flash(t0 + ms(5));
        assert_eq!(g.wd.trick_flash_phase(t0 + ms(5)), TrickFlashPhase::Idle);
        g.echo(" ", t0 + ms(10));
        assert!(g.frame(t0 + ms(300)).0.is_empty());
    }

    /// Reduced motion can never arm the wake that would take a transient tint
    /// off again, and ink off means zero ink for everyone: no flash, nothing
    /// armed, and no slot left waiting — before the echo or in mid-flash.
    #[test]
    fn reduced_motion_and_ink_off_emit_nothing_and_never_arm() {
        let gates: [fn(&mut DecoConfig); 2] = [
            |cfg| cfg.reduced_motion = true,
            |cfg| cfg.ink_enabled = false,
        ];
        for path in BOTH {
            for gate in gates {
                let t0 = Instant::now();
                let mut g = Glass::new(path, 4, 40);
                gate(&mut g.cfg);
                g.echo("$ sit", t0);
                g.fire("sit", 0, " ", t0);
                assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Idle);
                let (ink, fp) = g.frame(t0 + ms(300));
                assert!(ink.is_empty() && fp == 0, "{path:?}");
                assert!(!g.wd.is_active(t0), "{path:?}: never armed");

                // The gate closing MID-FLASH (a reload, a window losing focus).
                let mut g = Glass::new(path, 4, 40);
                g.echo("$ sit", t0);
                g.fire("sit", 0, " ", t0);
                assert_eq!(g.frame(t0 + ms(300)).0.len(), 3);
                gate(&mut g.cfg);
                let (ink, fp) = g.frame(t0 + ms(316));
                assert!(ink.is_empty() && fp == 0, "{path:?}");
                assert!(!g.wd.is_active(t0 + ms(316)), "{path:?}: disarmed at once");
                assert_eq!(g.wd.trick_flash_phase(t0 + ms(316)), TrickFlashPhase::Idle);
            }
        }
    }

    /// The grid holds BASE scalars only: Thai `นั่ง` and Hindi `बैठ` are typed
    /// with combining marks the cells never show as cells of their own.
    #[test]
    fn thai_and_hindi_words_are_found_under_their_combining_marks() {
        for path in BOTH {
            for word in ["นั่ง", "बैठ"] {
                let t0 = Instant::now();
                let mut g = Glass::new(path, 4, 40);
                g.echo(&format!("$ {word}"), t0);
                assert_eq!(g.caret(), (0, 4), "{word}: two cells on the glass");
                g.fire(word, 0, " ", t0);
                assert_eq!(
                    cells_at(&g.frame(t0 + ms(300)).0),
                    [(0, 2), (0, 3)],
                    "{path:?}: {word}"
                );
            }
        }
    }

    /// The shipping GUI lays an RTL run out in VISUAL order — reversed in
    /// place — while the typed token is logical. Both spellings are tried for
    /// an RTL word; a left-to-right word is never mirrored.
    #[test]
    fn a_hebrew_word_is_found_in_visual_order_and_in_logical_order() {
        for path in BOTH {
            for on_glass in ["שב", "בש"] {
                let t0 = Instant::now();
                let mut g = Glass::new(path, 4, 40);
                g.echo(&format!("$ {on_glass}"), t0);
                g.fire("שב", 0, " ", t0);
                assert_eq!(
                    cells_at(&g.frame(t0 + ms(300)).0),
                    [(0, 2), (0, 3)],
                    "{path:?}: typed שב, the glass shows {on_glass}"
                );
                // The mirrored match keeps verifying on later rescans.
                g.scan(t0 + ms(310));
                assert_eq!(g.frame(t0 + ms(320)).0.len(), 2, "{path:?}");
            }
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo("$ tis", t0);
            g.fire("sit", 0, " ", t0);
            assert!(
                g.frame(t0 + ms(300)).0.is_empty(),
                "{path:?}: `tis` is not `sit`"
            );
        }
    }

    /// Cyrillic is East-Asian-AMBIGUOUS: under `ambiguous_width_double` every
    /// letter is two cells, so a width computed from the typed word would be
    /// half the real span. The columns come from the matched cells instead.
    #[test]
    fn a_russian_word_is_found_under_ambiguous_double_width() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.term.apply_config(&aterm_core::config::TerminalConfig {
                ambiguous_width_double: true,
                ..Default::default()
            });
            g.echo("$ сидеть", t0);
            assert_eq!(g.caret(), (0, 14), "six letters, twelve cells");
            g.fire("сидеть", 0, " ", t0);
            assert_eq!(
                cells_at(&g.frame(t0 + ms(300)).0),
                [(0, 2), (0, 4), (0, 6), (0, 8), (0, 10), (0, 12)],
                "{path:?}: LEAD cells only"
            );
        }
    }

    /// One IME commit: `ねこ、おすわり！` arrives whole, the fire happens at `！`
    /// BEFORE any of it is echoed, so the recorded caret is where the commit
    /// STARTS and the word lies to its RIGHT — three wide characters along.
    #[test]
    fn a_japanese_word_is_found_on_its_lead_cells_inside_one_ime_commit() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo("$ ", t0);
            assert_eq!(g.caret(), (0, 2));
            g.fire("おすわり", 3, "ねこ、おすわり！", t0);
            // `ねこ` is a feline word and glows on its own account (cols 2-5).
            let flashed: Vec<(u16, u16)> = cells_at(&g.frame(t0 + ms(300)).0)
                .into_iter()
                .filter(|(_, col)| *col >= 6)
                .collect();
            assert_eq!(
                flashed,
                [(0, 8), (0, 10), (0, 12), (0, 14)],
                "{path:?}: lead cells of the four wide glyphs"
            );
        }
    }

    /// WHOLE WORD: `sit` inside `site` or `visit` is a piece of someone's prose.
    #[test]
    fn a_piece_of_a_longer_word_is_never_the_typed_word() {
        for path in BOTH {
            for (typed, rest) in [("$ sit", "e "), ("$ visit", " ")] {
                let t0 = Instant::now();
                let mut g = Glass::new(path, 4, 40);
                g.echo(typed, t0);
                g.fire("sit", 0, rest, t0);
                assert!(
                    g.frame(t0 + ms(300)).0.is_empty(),
                    "{path:?}: {typed}{rest}"
                );
                assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
            }
        }
    }

    /// `meow` is a trick word AND a feline word, so the flash lands exactly on
    /// cells the occurrence is already inking with its self-glow. The merge
    /// REPLACES (one cell per `(row, col)`, still sorted), and it mixes from
    /// the glow's colour, so the fade lands on what shows without the flash.
    #[test]
    fn the_merge_keeps_ink_sorted_unique_over_an_overlapping_self_glow() {
        let t0 = Instant::now();
        let script = |fire: bool| {
            let mut g = Glass::new(Path::Cold, 4, 40);
            g.echo("a kitten naps\r\n$ \x1b[38;2;60;120;200mmeow", t0);
            if fire {
                g.wd.note_trick_typed(t0, None, "meow", 0);
            }
            g.echo(" ", t0);
            g
        };
        let (mut plain, mut flashed) = (script(false), script(true));
        let caret = (1u16, 6u16);
        for t in [350u64, 500, 1000, 1099] {
            let (glow, _) = plain.frame(t0 + ms(t));
            let (ink, _) = flashed.frame(t0 + ms(t));
            assert!(sorted_unique(&ink), "t = {t}");
            assert_eq!(
                cells_at(&ink),
                cells_at(&glow),
                "t = {t}: the flash added no second cell to an inked glyph"
            );
            assert_eq!(glow.len(), 6 + 4, "kitten + meow, both glowing");
            for (i, (got, base)) in ink.iter().zip(&glow).skip(6).enumerate() {
                assert_ne!(base.color, [60, 120, 200], "the glow really moved it");
                assert_eq!(
                    got.color,
                    law(
                        caret,
                        "meow",
                        i,
                        t,
                        base.color,
                        [60, 120, 200],
                        plain.row(1)[2].bg
                    ),
                    "t = {t}, cell {i}: the base is the GLOW's colour"
                );
            }
            // The other word's ink is untouched.
            assert_eq!(ink[..6], glow[..6], "t = {t}");
        }
    }

    /// Flash cells are INSERTED in `(row, col)` order between other rows' ink.
    #[test]
    fn the_flash_inserts_between_other_rows_ink_in_order() {
        let t0 = Instant::now();
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("kitten\r\n\r\nkitty\x1b[2;1H$ sit", t0);
        assert_eq!(g.caret(), (1, 5));
        g.fire("sit", 0, " ", t0);
        let (ink, _) = g.frame(t0 + ms(350));
        assert!(sorted_unique(&ink));
        let rows: Vec<u16> = ink.iter().map(|cell| cell.row).collect();
        assert_eq!(rows, [0, 0, 0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 2, 2]);
    }

    /// A flash that only just started is not cut off by the next one (the new
    /// word is skipped); once it is past its opening third of a second, the
    /// new word replaces it. Cosmetic — the safety is in the const asserts.
    #[test]
    fn a_second_flash_inside_the_opening_window_is_skipped_and_a_later_one_replaces() {
        let t0 = Instant::now();
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("$ sit", t0);
        g.fire("sit", 0, " ", t0);
        let early = t0 + ms(u64::from(MIN_ANIMATION_MS) - 1);
        g.echo("jump", early);
        g.fire("jump", 0, " ", early);
        assert_eq!(
            cells_at(&g.frame(early + ms(10)).0),
            [(0, 2), (0, 3), (0, 4)],
            "still `sit`: the second note was skipped"
        );
        let later = t0 + ms(u64::from(MIN_ANIMATION_MS));
        g.echo("play", later);
        g.fire("play", 0, " ", later);
        assert_eq!(
            cells_at(&g.frame(later + ms(200)).0),
            [(0, 11), (0, 12), (0, 13), (0, 14)],
            "`play` replaced it"
        );
        // The debounce clock outlives the slot: revoke-then-refire cannot strobe.
        g.wd.revoke_trick_flash(later + ms(10));
        let gone = later + ms(10 + u64::from(REVOKE_FADE_MS));
        assert!(g.frame(gone).0.is_empty());
        g.echo("roll", gone);
        g.fire("roll", 0, " ", gone);
        assert!(
            g.frame(gone + ms(50)).0.is_empty(),
            "inside `play`'s opening window"
        );
    }

    /// A located flash FOLLOWS its line by the reported scroll, and with no
    /// report it ENDS rather than search again: the shell's error line holds
    /// the same characters in the same columns one row away.
    #[test]
    fn a_flash_follows_its_line_as_output_scrolls_it() {
        let prompt = format!("{}$ ", "~".repeat(22));
        for path in BOTH {
            for wired in [true, false] {
                let t0 = Instant::now();
                let mut g = Glass::new(path, 4, 40);
                g.wired = wired;
                g.echo(&format!("one\r\ntwo\r\nthree\r\n{prompt}sit"), t0);
                g.fire("sit", 0, " ", t0);
                assert_eq!(cells_at(&g.frame(t0 + ms(100)).0)[0], (3, 24));
                g.echo(
                    &format!("\r\nzsh: command not found: sit\r\n{prompt}"),
                    t0 + ms(200),
                );
                let (ink, _) = g.frame(t0 + ms(300));
                if wired {
                    assert_eq!(
                        cells_at(&ink),
                        [(1, 24), (1, 25), (1, 26)],
                        "{path:?}: followed two rows up"
                    );
                } else {
                    assert!(ink.is_empty(), "{path:?}: ended, never re-searched");
                }
            }
        }
    }

    /// Text gone, or a different grid width: the flash ends at that rescan.
    #[test]
    fn a_flash_ends_when_its_text_is_erased_or_the_grid_changes_width() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo("$ sit", t0);
            g.fire("sit", 0, " ", t0);
            assert_eq!(g.frame(t0 + ms(100)).0.len(), 3);
            // Ctrl+U: the shell redraws an empty prompt.
            g.echo("\r\x1b[K$ ", t0 + ms(110));
            let (ink, fp) = g.frame(t0 + ms(116));
            assert!(ink.is_empty() && fp == 0, "{path:?}: text gone");
            assert!(!g.wd.is_active(t0 + ms(116)));

            let mut g = Glass::new(path, 4, 40);
            g.echo("$ sit", t0);
            g.fire("sit", 0, " ", t0);
            assert_eq!(g.frame(t0 + ms(100)).0.len(), 3);
            g.term.resize(4, 50);
            g.cols = 50;
            g.scan(t0 + ms(110));
            assert!(
                g.frame(t0 + ms(116)).0.is_empty(),
                "{path:?}: width changed"
            );
        }
    }

    /// A pane the engine has never scanned with a cursor has no caret to
    /// anchor on: the note stores nothing (it must not search the screen).
    #[test]
    fn a_pane_with_no_caret_observation_stores_no_flash() {
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        wd.note_trick_typed(t0, None, "sit", 0);
        assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Idle);
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("$ sit", t0);
        g.wd.note_trick_typed(t0, Some(77), "sit", 0);
        assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
        g.wd.bind_pane(5, (0, 0));
        g.wd.revoke_trick_flash(t0);
        g.wd.note_trick_typed(t0, Some(99), "sit", 0);
        assert_eq!(
            g.wd.trick_flash_phase(t0),
            TrickFlashPhase::Idle,
            "pane 99 was never scanned"
        );
        // Degenerate words store nothing either.
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("$ ", t0);
        for word in ["", "\u{200d}\u{fe0f}", &"x".repeat(MAX_CHARS + 1)] {
            g.wd.note_trick_typed(t0, None, word, 0);
            assert_eq!(
                g.wd.trick_flash_phase(t0),
                TrickFlashPhase::Idle,
                "{word:?}"
            );
        }
        g.wd.note_trick_typed(t0, None, &"x".repeat(MAX_CHARS), 0);
        assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
    }

    /// The per-pane base-y memo is bounded, withdrawn with the declaration,
    /// pruned with the panes — and a flash naming a departed pane goes too.
    #[test]
    fn the_base_y_memo_is_bounded_and_pruned_with_the_panes() {
        let t0 = Instant::now();
        let mut flash = TrickFlash::default();
        let mut buf = Vec::new();
        let mut scan = |flash: &mut TrickFlash, pane: u64, base_y: Option<i64>, cursor| {
            flash.declare_base_y(Some(pane), base_y);
            flash.on_rescan(Some(pane), t0, 4, 40, cursor, true, &mut buf, |_, row| {
                row.clear();
            });
        };
        for pane in 0..40u64 {
            scan(&mut flash, pane, Some(pane as i64), Some((0, 0)));
        }
        assert_eq!(flash.base_y.len(), BASE_Y_MEMO_CAP);
        assert_eq!(flash.base_y[0], (Some(24), 24), "oldest-first eviction");
        // A cursor-less rescan is not an observation of the caret's row origin.
        scan(&mut flash, 39, Some(1000), None);
        assert!(flash.base_y.contains(&(Some(39), 39)));
        // Re-declaring updates in place; withdrawing removes.
        scan(&mut flash, 39, Some(41), Some((0, 0)));
        assert!(flash.base_y.contains(&(Some(39), 41)));
        scan(&mut flash, 39, None, Some((0, 0)));
        assert!(flash.base_y.iter().all(|(pane, _)| *pane != Some(39)));
        // One pane's declaration never stands in for another pane's rescan.
        flash.declare_base_y(Some(30), Some(7));
        flash.on_rescan(
            Some(31),
            t0,
            4,
            40,
            Some((0, 0)),
            true,
            &mut buf,
            |_, row| {
                row.clear();
            },
        );
        assert!(flash.base_y.iter().all(|(pane, _)| *pane != Some(31)));

        flash.note(t0, Some(38), Some(38), Some((0, 0)), "sit", 0);
        assert_eq!(flash.slot.map(|slot| slot.base_y), Some(Some(38)));
        flash.retain_panes(|pane| pane < 30);
        assert!(
            flash
                .base_y
                .iter()
                .all(|(pane, _)| pane.is_some_and(|p| p < 30))
        );
        assert!(!flash.base_y.is_empty());
        assert!(
            flash.slot.is_none(),
            "pane 38 is gone, and its flash with it"
        );
    }

    /// The legibility guard holds the bound at EVERY hue, not only at the six
    /// it samples; it binds where it should (pure blue on a dark theme, pure
    /// yellow on a light one — the two corners a 45° grid misses); and text the
    /// theme itself draws below the bound is not tinted at all, which ENDS the
    /// flash rather than arming a second of frames that show nothing.
    #[test]
    fn the_legibility_guard_holds_the_contrast_bound_at_every_hue() {
        let contrast = |a: u32, b: u32| {
            let (la, lb) = (relative_luminance(a), relative_luminance(b));
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        };
        let themes: [(u32, u32); 7] = [
            (0x00E6_E6E6, 0x0000_0000), // light on black
            (0x00FF_FFFF, 0x001E_1E2E), // white on a dark blue
            (0x001E_1E1E, 0x00FA_FAFA), // dark on near-white
            (0x0000_0000, 0x00FF_FFFF), // black on white
            (0x0060_6060, 0x0000_0000), // dim comment grey: BELOW the bound
            (0x00FF_5555, 0x0028_2A36), // an "unknown command" red
            (0x0080_8080, 0x0080_8080), // concealed: fg == bg
        ];
        for (fg, bg) in themes {
            let (sat, val) = resolve_sat_val(bg);
            let strength = legible_strength(fg, bg, sat, val);
            assert!((0.0..=FLASH_STRENGTH).contains(&strength));
            // Whatever the text's own contrast is, the tint never goes below
            // the bound — or, where the text is already below it, below the text.
            let floor = MIN_INK_CONTRAST.min(contrast(fg, bg));
            for hue in 0..360 {
                let ink = mix_rgb(fg, hsv2rgb(hue as f32, sat, val), strength);
                assert!(
                    contrast(ink, bg) >= floor - 0.02,
                    "fg {fg:06x} on {bg:06x} at {hue}°: {} < {floor}",
                    contrast(ink, bg)
                );
            }
        }
        let (dark, light) = (themes[0], themes[2]);
        for (fg, bg) in [dark, light] {
            let (sat, val) = resolve_sat_val(bg);
            let strength = legible_strength(fg, bg, sat, val);
            assert!(
                strength > 0.0 && strength < FLASH_STRENGTH,
                "{fg:06x} on {bg:06x}: the guard must bind without erasing ({strength})"
            );
        }
        for (fg, bg) in [themes[4], themes[6]] {
            let (sat, val) = resolve_sat_val(bg);
            assert_eq!(
                legible_strength(fg, bg, sat, val),
                0.0,
                "{fg:06x} on {bg:06x}: below the bound already, so never tinted"
            );
        }
        // …and on the glass that is NO FLASH, not an armed second of nothing —
        // so a flash can never reveal concealed text either.
        for sgr in ["\x1b[38;2;96;96;96m\x1b[48;2;0;0;0m", "\x1b[8m"] {
            for path in BOTH {
                let t0 = Instant::now();
                let mut g = Glass::new(path, 4, 40);
                g.echo(&format!("$ {sgr}sit"), t0);
                g.fire("sit", 0, " ", t0);
                assert_eq!(g.wd.trick_flash_phase(t0), TrickFlashPhase::Idle);
                let (ink, fp) = g.frame(t0 + ms(300));
                assert!(ink.is_empty() && fp == 0, "{path:?}: {sgr:?}");
                assert!(!g.wd.is_active(t0), "{path:?}: never armed");
            }
        }
    }

    /// PHOTOSENSITIVITY, MEASURED off the real colour law (the declared rate
    /// is const-asserted at build time; this is the part a constant cannot
    /// state). Sample the relative luminance of one cell at 1 kHz across the
    /// whole flash, over every opening hue, word position, theme and strength,
    /// and count LARGE direction reversals — WCAG 2.3.1's unit: an opposing
    /// change of at least 10% of the maximum relative luminance.
    #[test]
    fn the_flash_stays_under_the_photosensitivity_bound() {
        /// Direction changes of at least `0.1`, by hysteresis on the running
        /// extreme (a wiggle smaller than the threshold is not a flash).
        fn reversals(lums: &[f32]) -> u32 {
            let (mut dir, mut extreme, mut count) = (0i8, lums[0], 0u32);
            for &lum in lums {
                let moved = lum - extreme;
                if dir == 0 {
                    if moved.abs() >= 0.1 {
                        dir = if moved > 0.0 { 1 } else { -1 };
                        extreme = lum;
                    }
                } else if moved * f32::from(dir) > 0.0 {
                    extreme = lum;
                } else if moved.abs() >= 0.1 {
                    dir = -dir;
                    extreme = lum;
                    count += 1;
                }
            }
            count
        }
        let themes: [(u32, u32); 5] = [
            (0x00FF_FFFF, 0x0000_0000),
            (0x00E6_E6E6, 0x001E_1E2E),
            (0x0080_8080, 0x0000_0000),
            (0x0000_0000, 0x00FF_FFFF),
            (0x001E_1E1E, 0x00FA_FAFA),
        ];
        let mut worst = 0u32;
        for (fg, bg) in themes {
            let (sat, val) = resolve_sat_val(bg);
            // The guarded strength the emitter would use, AND the unguarded
            // peak — the bound must not depend on the guard having bound.
            for strength in [legible_strength(fg, bg, sat, val), FLASH_STRENGTH, 1.0] {
                for hue0 in (0..360).step_by(5) {
                    for (i, n) in [(0usize, 1usize), (0, 4), (3, 4), (7, 8)] {
                        let lums: Vec<f32> = (0..=u64::from(FLASH_MS))
                            .map(|t| {
                                let target = hsv2rgb(flash_hue(hue0 as f32, i, n, t), sat, val);
                                relative_luminance(mix_rgb(
                                    fg,
                                    target,
                                    envelope(t, None) * strength,
                                ))
                            })
                            .collect();
                        worst = worst.max(reversals(&lums));
                    }
                }
            }
        }
        assert!(
            worst as f32 <= FLASH_REVERSALS,
            "measured {worst} large reversals in one flash against a declared {FLASH_REVERSALS}"
        );
        let hz = worst as f32 * 1000.0 / FLASH_MS as f32;
        assert!(
            hz <= TWINKLE_FLASH_BOUND_HZ,
            "the flash reverses luminance {hz} times a second — over the {TWINKLE_FLASH_BOUND_HZ} Hz bound"
        );
        assert!(
            worst >= 2,
            "non-vacuous: a hue revolution DOES cross the cliffs"
        );
        // A revoke only ever shortens the flash: it adds no reversal.
        let (fg, bg) = themes[0];
        let (sat, val) = resolve_sat_val(bg);
        for revoked in [0u64, 60, 130, 400, 900] {
            let lums: Vec<f32> = (0..=revoked + u64::from(REVOKE_FADE_MS))
                .map(|t| {
                    let target = hsv2rgb(flash_hue(240.0, 0, 1, t), sat, val);
                    relative_luminance(mix_rgb(fg, target, envelope(t, Some(revoked))))
                })
                .collect();
            assert!(
                reversals(&lums) as f32 <= FLASH_REVERSALS,
                "revoked at {revoked}"
            );
            assert_eq!(
                envelope(revoked + u64::from(REVOKE_FADE_MS), Some(revoked)),
                0.0
            );
        }
    }
}
