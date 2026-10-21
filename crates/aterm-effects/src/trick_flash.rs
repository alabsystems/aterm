// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The trick flash: the word a person just typed to the pet (`sit`, the `good`
//! of `good kitty`) turns rainbow for about a second, then its glyphs return to
//! exactly the colour they had.
//!
//! Pet commands are ordinary verbs that fill build logs and READMEs, so they are
//! neither a scanned lexicon class nor the rainbow colorway (which settles to a
//! permanent tint). The typed-line listener ([`crate::typed_tricks`]) decides a
//! word was a command, the host says which word and how far behind the caret it
//! sits, and this module finds it on the glass, paints it and lets go. One slot.
//!
//! * Typed, never seen: a slot exists only because the input path noted one, so
//!   `echo sit`, a pasted `sit` or a file full of `sit` never ink.
//! * Never paint a place not found: the note carries a caret and a word, not a
//!   cell. It becomes paintable only when a rescan of the same pane finds those
//!   characters as a whole word within reach of that caret, on the caret's row
//!   moved by exactly the scroll the snapshot reports. Unfound after
//!   `GIVE_UP_MS`, it is dropped.
//! * No residue: the colour is `mix(base, rainbow, env(t) · strength)` with
//!   `env` zero at both ends, and after the window the flash emits zero cells.
//!   Occurrence ink already on the word (a custom spec, a feline self-glow) is
//!   the base.
//!
//! A cell holds only its base scalar (Thai `นั่ง` is the two cells `น ง`), the
//! GUI lays an RTL run out in visual order, and an East-Asian-ambiguous letter is
//! two cells under `ambiguous_width_double`. So the typed word is stored without
//! zero-width scalars, an RTL word is also tried mirrored, and every column comes
//! from the matched cells.
//!
//! The row is the hard part: `sit⏎` on the bottom row scrolls before its echo is
//! scanned, and the shell's `command not found: sit` then sits one row above the
//! typed line. A host that declares `base_y` (`TrickFlash::declare_base_y`) gets
//! `row = caret.row − (base_y_now − base_y_then)`, and only that row is tested;
//! a bounded upward search covers only scrolls the snapshot cannot see (a scroll
//! region), under the conditions on `locate`.
//!
//! On that row the caret picks the copy ([`Anchor`]): one it anchors exactly,
//! then one it stands inside (recorded mid-echo), then one it only reaches,
//! taken only with the live cursor just past it. So `sit sit␣` typed in one
//! burst flashes the copy that fired, and under an unreported scroll the shell's
//! `bash: sit: command not found` on the caret's row does not flash unless the
//! prompt's width lines its `sit` up with the caret.
//!
//! The slot is window-scoped (a keyboard has one owner) and names its pane, which
//! every hook compares to the scan scope. Flash safety is structural: one hue
//! revolution per flash, with the luminance-reversal rate const-asserted under
//! the WCAG 2.3.1 bound; the start debounce is cosmetic. With no slot the cost is
//! one `Option` test per rescan and tick; a slot costs one row copy per rescan
//! and at most 32 cells per frame. The note, the revoke and a flashing frame
//! never allocate (`tests/trick_flash_alloc.rs`).

use std::time::Duration;

use aterm_core::terminal::RenderCell;
use aterm_hash::{FNV64_OFFSET, fnv_mix_u64};
use aterm_lexicon::{TrickLexicon, is_no_space_script, is_token_char};
use aterm_render::InkCell;
use aterm_scene::{mix_rgb, smoothstep};
use aterm_time::Instant;

use crate::color_math::hsv2rgb;
#[cfg(test)]
use crate::color_math::relative_luminance;
use crate::effect_util::TWINKLE_FLASH_BOUND_HZ;
use crate::spec::{MIN_ANIMATION_MS, RAINBOW_SAT, RAINBOW_SPAN_DEG, RAINBOW_VAL};
use crate::word_decorations::{
    INK_FADE_MS, INK_RAMP_IN_MS, MIN_INK_CONTRAST, TYPED_EDIT_REACH_COLS, TYPED_EDIT_WITNESS_MS,
    fold_ink, rainbow_base_hue, rgb3_to_u32, u32_to_rgb3,
};

/// The whole flash in ms (ramp in, hold, fade out), also the hue cycle's period:
/// one revolution per flash, so no cell crosses the yellow-to-blue luminance
/// cliff more than once. Crate-visible for the engine's hook tests.
pub(crate) const FLASH_MS: u32 = 1100;
/// Smooth ramp-in, ms (the ink sweep's own ramp).
const RAMP_MS: u32 = 130;
/// Smoothstep fade over the last this-many ms, back to the base colour.
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
/// Peak tint toward the rainbow before the legibility guard. The flash owns this
/// (it never reads the shared ink strength knob).
const FLASH_STRENGTH: f32 = 0.9;
/// The typed word's inline buffer is the listener's token window: a word the
/// vocabulary could hold always fits, and a longer one is not a trick.
const MAX_CHARS: usize = TrickLexicon::MAX_SURFACE_CHARS;
/// Panes whose last cursor-bearing `base_y` is remembered. A window shows a
/// handful; past the cap the oldest entry leaves, which costs that pane one
/// flash's scroll-following and nothing else.
const BASE_Y_MEMO_CAP: usize = 16;

/// Large luminance reversals one flash can show on one cell: off the base on the
/// ramp, across the revolution's two cliffs (up to yellow, down to blue), back to
/// the base on the fade, so at most three changes of direction. Pinned by
/// `the_flash_stays_under_the_photosensitivity_bound`.
const FLASH_REVERSALS: f32 = 3.0;
/// That count as a rate over the flash, Hz.
const FLASH_REVERSAL_HZ: f32 = FLASH_REVERSALS * 1000.0 / FLASH_MS as f32;

/// Photosensitivity bounds checked at build time (as in `effect_util`): the
/// hue cycle may not run faster than the animation floor; ramp and fade may not
/// be sharper than the ink sweep's; the envelope must fit both; a revoke may
/// hurry the fade but never cut; and the declared reversal rate stays under the
/// general-flash bound.
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

/// Where the flash slot stands at `now`: the lifecycle the formal model binds.
/// A pure function of slot and clock, so a pending slot past its give-up bound
/// or a live one past its window reads [`Idle`](Self::Idle) before a rescan or
/// tick collects it.
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
    /// The pre-echo caret of that pane, `(row, col)`.
    caret: (u16, u16),
    /// That pane's `base_y` at the scan which recorded the caret, when the
    /// host declares one.
    base_y: Option<i64>,
    /// The typed word, zero-width scalars removed, in logical order.
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
    /// The envelope's zero: `max(rescan clock, key time)`, so a late echo still
    /// ramps in instead of popping in at full rainbow.
    since: Instant,
    row: u16,
    base_y: Option<i64>,
    /// The grid width it was found under; a different width ends the flash.
    grid_cols: u16,
    /// The match was the mirrored (visual-order) spelling.
    mirrored: bool,
    /// Lead cells only, ascending: a wide glyph's lead governs both halves.
    cols: [u16; MAX_CHARS],
    fg: [[u8; 3]; MAX_CHARS],
    bg: [u8; 3],
    /// Resolved for the word's background (light backgrounds take deep tones).
    sat: f32,
    val: f32,
    /// The legibility-guarded peak tint, computed at locate time rather than per
    /// frame: a per-frame guard would step in eighths as the hue rotates, visibly.
    strength: f32,
}

/// The flash state a word engine owns. Every method takes the scan scope, caret
/// and cells by argument, so the engine's own `impl` stays in its file.
#[derive(Debug, Default)]
pub(crate) struct TrickFlash {
    slot: Option<Slot>,
    /// When the last flash became visible — the cosmetic debounce's clock. It
    /// outlives the slot so revoke-then-refire cannot strobe either.
    last_start: Option<Instant>,
    /// What the host declared for its next rescan, and for which scan scope:
    /// scope-keyed so one pane's declaration never serves another's rescan, and
    /// taken by the rescan that reads it so it never serves a later snapshot.
    declared: Option<(Option<u64>, i64)>,
    /// Per-pane `base_y` as of that pane's last cursor-bearing rescan, the same
    /// moment its caret is recorded, so a note reads one observation.
    base_y: [(Option<u64>, i64); BASE_Y_MEMO_CAP],
    base_y_len: u8,
}

impl TrickFlash {
    fn base_y_slice(&self) -> &[(Option<u64>, i64)] {
        &self.base_y[..usize::from(self.base_y_len)]
    }

    /// The host's declaration for the one rescan it is about to run in `scope`,
    /// which spends it. `None` withdraws it: every scroll delta then reads zero.
    pub(crate) fn declare_base_y(&mut self, scope: Option<u64>, base_y: Option<i64>) {
        self.declared = base_y.map(|y| (scope, y));
    }

    /// Forget panes the window no longer shows: their memo entries, and a slot
    /// naming one (no rescan will collect it).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickFlashLifecycle",
            action = "CloseOwn",
            project = "conformance_trick_flash::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickFlashLifecycle",
            action = "CloseOther",
            project = "conformance_trick_flash::project"
        )
    )]
    pub(crate) fn retain_panes(&mut self, keep: impl Fn(u64) -> bool) {
        let mut w = 0usize;
        for r in 0..usize::from(self.base_y_len) {
            let entry = self.base_y[r];
            if entry.0.is_none_or(&keep) {
                self.base_y[w] = entry;
                w += 1;
            }
        }
        self.base_y_len = w as u8;
        if self
            .declared
            .is_some_and(|(pane, _)| pane.is_some_and(|pane| !keep(pane)))
        {
            self.declared = None;
        }
        if self
            .slot
            .is_some_and(|slot| slot.pane.is_some_and(|pane| !keep(pane)))
        {
            self.slot = None;
        }
    }

    /// Whether the slot names the pane its key went to and was found there. Such
    /// a flash is that pane's, not the window-wide grid's, so a bind from unbound
    /// keeps it across the reset. An unfound word's caret may be the window-wide
    /// grid's (the unsplit host names its session too), whose geometry the bind
    /// just changed, so it goes; a found one ends by itself if its columns move.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickFlashLifecycle",
            action = "Rebind",
            project = "conformance_trick_flash::project"
        )
    )]
    pub(crate) fn found_in_a_pane(&self) -> bool {
        self.slot
            .is_some_and(|slot| slot.pane.is_some() && slot.located.is_some())
    }

    /// Drop the flash. Idempotent, so the engine's per-pane reset may call it per
    /// pane. The debounce clock and memo observe the keyboard and grid, and stay.
    pub(crate) fn clear(&mut self) {
        self.slot = None;
    }

    /// The input path says `word` was just typed to the pet in `pane`, with the
    /// pre-echo `caret` and `back_chars` typed characters between the word and
    /// that caret. `scope` is the engine's current scan scope, the memo key for a
    /// host naming no pane.
    ///
    /// Fail-silent: no caret observation, an empty or over-long word, or a flash
    /// that only just started stores nothing. O(len) into a fixed inline buffer;
    /// never allocates.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickFlashLifecycle",
            action = "NotePane",
            project = "conformance_trick_flash::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickFlashLifecycle",
            action = "NoteWindow",
            project = "conformance_trick_flash::project"
        )
    )]
    pub(crate) fn note(
        &mut self,
        now: Instant,
        pane: Option<u64>,
        scope: Option<u64>,
        caret: Option<(u16, u16)>,
        word: &str,
        back_chars: u16,
    ) {
        // A pane never scanned with a cursor has no caret to anchor on; the only
        // alternative would be searching the whole screen.
        let Some(caret) = caret else {
            return;
        };
        // Cosmetic debounce: a flash in its opening third of a second is not cut
        // off; the new word is skipped. Not a safety limiter.
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
            // The grid keeps a zero-width scalar on the previous cell, so it can
            // never be found as a cell of its own.
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
            .base_y_slice()
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

    /// The line was not pet talk. A flash on the glass leaves over
    /// [`REVOKE_FADE_MS`]; one never found is dropped. Idempotent (the first
    /// stamp stands, so the fade cannot restart).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickFlashLifecycle",
            action = "Revoke",
            project = "conformance_trick_flash::project"
        )
    )]
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

    /// The rescan half, called by both rescan variants after their scan pass:
    /// remember this pane's `base_y`, then locate a pending slot or re-verify a
    /// located one. `load_row` fills `row_buf` with one viewport row.
    ///
    /// `paintable` is `ink on && !reduced motion`; when false the slot is dropped.
    #[allow(
        clippy::too_many_arguments,
        reason = "the rescan hands over its scope, clock, geometry, caret, gate and row source in one call; a carrier struct would rename the list, not shrink it"
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickFlashLifecycle",
            action = "Find",
            project = "conformance_trick_flash::project"
        )
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
        // Spent here whatever its scope: a declaration describes one snapshot. An
        // undeclared later rescan reads "no row origin", never the last one,
        // which would pair this caret with a pre-scroll `base_y`.
        let base_y_now = self
            .declared
            .take()
            .and_then(|(declared_for, y)| (declared_for == scope).then_some(y));
        if cursor.is_some() {
            self.remember_base_y(scope, base_y_now);
        }
        // The one test an engine with no flash pays.
        let Some(mut slot) = self.slot else {
            return;
        };
        if !pane_matches(slot.pane, scope) {
            return;
        }
        // A rescan of the flash's pane without a cursor is a scrollback view:
        // viewport rows are no longer live, so the slot ends.
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

    /// The tick half: merge this frame's flash cells into `ink` (kept
    /// sorted-unique by `(row, col)`), fold them and the flash clock into `fp`,
    /// and return when the flash ends so the caller arms its own deadline.
    /// `None` means nothing was painted and `ink`/`fp` are unchanged.
    ///
    /// Where `ink` already holds a cell, that colour is the mix base, so the fade
    /// lands on what would have shown without the flash.
    pub(crate) fn emit(
        &mut self,
        scope: Option<u64>,
        now: Instant,
        paintable: bool,
        ink: &mut Vec<InkCell>,
        fp: &mut u64,
    ) -> Option<Instant> {
        // The one test a frame with no flash pays.
        let slot = self.slot?;
        if !pane_matches(slot.pane, scope) {
            return None;
        }
        if !paintable {
            self.slot = None;
            return None;
        }
        // Never paint a place not found.
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
        let steps = n.saturating_sub(1) as f32;
        let span = RAINBOW_SPAN_DEG.min(100.0 * steps);
        let denom = steps.max(1.0);
        let time_deg = 360.0 * (t as f32 / FLASH_MS as f32);
        let mut search_from = 0usize;
        for i in 0..n {
            let hue = (hue0 + (i as f32 / denom) * span + time_deg).rem_euclid(360.0);
            let target = hsv2rgb(hue, loc.sat, loc.val);
            let col = loc.cols[i];
            let at = ink[search_from..].binary_search_by_key(&(loc.row, col), |c| (c.row, c.col));
            let cell = match at {
                Ok(rel) => {
                    let j = search_from + rel;
                    search_from = j + 1;
                    ink[j].color = u32_to_rgb3(mix_rgb(rgb3_to_u32(ink[j].color), target, tint));
                    ink[j]
                }
                Err(rel) => {
                    let j = search_from + rel;
                    search_from = j + 1;
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
        // The flash clock keeps the repaint early-out from skipping a live frame
        // (the wordless-screen hook has no frame counter to fold).
        *fp = fnv_mix_u64(*fp, t);
        Some(loc.since + Duration::from_millis(window))
    }

    fn remember_base_y(&mut self, scope: Option<u64>, base_y: Option<i64>) {
        let len = usize::from(self.base_y_len);
        let at = self.base_y[..len].iter().position(|(key, _)| *key == scope);
        match (at, base_y) {
            (Some(i), Some(y)) => self.base_y[i].1 = y,
            // The host stopped declaring: a stale row origin would be worse
            // than none.
            (Some(i), None) => {
                self.base_y.copy_within(i + 1..len, i);
                self.base_y_len -= 1;
            }
            (None, Some(y)) => {
                if len >= BASE_Y_MEMO_CAP {
                    self.base_y.copy_within(1..BASE_Y_MEMO_CAP, 0);
                    self.base_y[BASE_Y_MEMO_CAP - 1] = (scope, y);
                } else {
                    self.base_y[len] = (scope, y);
                    self.base_y_len += 1;
                }
            }
            (None, None) => {}
        }
    }
}

/// What decides a flash's opening hue: the word and where it was typed, and
/// nothing else — no clock and no counter, so a replay paints the same bytes.
fn hue_seed(caret: (u16, u16), chars: &[char]) -> u64 {
    let at = fnv_mix_u64(
        fnv_mix_u64(FNV64_OFFSET, u64::from(caret.0)),
        u64::from(caret.1),
    );
    chars.iter().fold(at, |h, c| fnv_mix_u64(h, u64::from(*c)))
}

/// The pane test every hook shares (and the typed-edit witness's): `None` on
/// either side names no pane (a single-grid host, or an engine with no declared
/// session), so there is nothing to cross; two named panes must be equal.
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
/// last [`FADE_MS`]; zero at both ends. A revoke at `revoked_t` multiplies in
/// its own short fade, continuous at the revoke, so revoking never pops.
fn envelope(t: u64, revoked_t: Option<u64>) -> f32 {
    let t = t as f32;
    let ramp = smoothstep(t / RAMP_MS as f32);
    let fade = 1.0 - smoothstep((t - (FLASH_MS - FADE_MS) as f32) / FADE_MS as f32);
    let revoke = revoked_t.map_or(1.0, |r| {
        1.0 - smoothstep((t - r as f32) / REVOKE_FADE_MS as f32)
    });
    ramp * fade * revoke
}

/// Lead cell `i` of `n`'s hue at `t` ms, in degrees: the opening hue, the word's
/// spatial spread (the colorway's span clamp: a two-step on a 2-cell word, a
/// pure temporal sweep on one cell), plus one revolution over the flash.
#[cfg(test)]
fn flash_hue(hue0: f32, i: usize, n: usize, t: u64) -> f32 {
    let steps = n.saturating_sub(1) as f32;
    let span = RAINBOW_SPAN_DEG.min(100.0 * steps);
    let u = i as f32 / steps.max(1.0);
    (hue0 + u * span + 360.0 * (t as f32 / FLASH_MS as f32)).rem_euclid(360.0)
}

/// The rainbow colorway's theme rule: a light background takes deep candy
/// tones that can clear the contrast bound on white.
fn resolve_sat_val(bg: u32) -> (f32, f32) {
    let table = crate::color_math::relative_luminance_table();
    if crate::color_math::relative_luminance_with(table, bg) > 0.5 {
        (1.0, 0.62)
    } else {
        (RAINBOW_SAT, RAINBOW_VAL)
    }
}

/// The legibility guard: the strongest tint, stepping down from
/// [`FLASH_STRENGTH`] in eighths, at which the flash is legible at every hue it
/// passes through.
///
/// Six samples cover the circle: between multiples of 60° exactly one RGB
/// channel moves linearly, the mix toward `fg` is linear per channel, and
/// relative luminance is monotone in each channel, so the tinted luminance is
/// monotone on each 60° arc and its extremes sit at the six corners (a 45° grid
/// misses the binding corners: pure blue on dark, pure yellow on light).
///
/// Two conditions per corner: contrast of at least [`MIN_INK_CONTRAST`] against
/// the word's background; and the tint staying on the text's side of the
/// background's luminance (corners on opposite sides would put a hue between
/// them exactly on the background).
///
/// Zero is a real answer: text the theme draws below the bound (dim comment
/// grey, concealed `fg == bg`) is not tinted, and the caller ends the flash
/// rather than arming frames that show nothing (so concealed text is never
/// revealed).
fn legible_strength(fg: u32, bg: u32, sat: f32, val: f32) -> f32 {
    let table = crate::color_math::relative_luminance_table();
    let lum_bg = crate::color_math::relative_luminance_with(table, bg);
    let contrast = |lum: f32| (lum.max(lum_bg) + 0.05) / (lum.min(lum_bg) + 0.05);
    let floor = MIN_INK_CONTRAST;
    let text_is_lighter = crate::color_math::relative_luminance_with(table, fg) >= lum_bg;
    let c = val * sat;
    let m = val - c;
    let hi = ((c + m).clamp(0.0, 1.0) * 255.0).round() as u32;
    let lo = (m.clamp(0.0, 1.0) * 255.0).round() as u32;
    let corners = [
        (hi << 16) | (lo << 8) | lo,
        (hi << 16) | (hi << 8) | lo,
        (lo << 16) | (hi << 8) | lo,
        (lo << 16) | (hi << 8) | hi,
        (lo << 16) | (lo << 8) | hi,
        (hi << 16) | (lo << 8) | hi,
    ];
    let step = FLASH_STRENGTH / 8.0;
    let mut strength = FLASH_STRENGTH;
    'guard: while strength > 0.0 {
        for corner in corners {
            let lum =
                crate::color_math::relative_luminance_with(table, mix_rgb(fg, corner, strength));
            if contrast(lum) < floor || (lum >= lum_bg) != text_is_lighter {
                strength = (strength - step).max(0.0);
                continue 'guard;
            }
        }
        break;
    }
    strength
}

/// A scalar inside a word on the glass: a token character of a spaced script,
/// or any no-space-script scalar. A match bounded by one of these is part of a
/// longer word (`sit` in `site`).
fn is_word_char(c: char) -> bool {
    is_token_char(c) || is_no_space_script(c)
}

/// Strong right-to-left scalars: Hebrew, Arabic and neighbours in the BMP, the
/// presentation forms, and the SMP's RTL planes. Only decides whether the
/// mirrored spelling is also tried, so a generous range is safe.
fn is_strong_rtl(c: char) -> bool {
    matches!(c as u32,
        0x0590..=0x08FF       // Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan, Mandaic, Arabic Ext
        | 0xFB1D..=0xFDFF     // Hebrew + Arabic presentation forms A
        | 0xFE70..=0xFEFF     // Arabic presentation forms B
        | 0x1_0800..=0x1_0FFF // Cypriot … Old Hungarian, Hanifi Rohingya, Sogdian
        | 0x1_E800..=0x1_EFFF // Mende Kikakui, Adlam, Arabic mathematical
    )
}

/// How well the recorded caret agrees with a match, weakest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Anchor {
    /// Within reach and nothing more: taken only with the live cursor just
    /// past the word.
    Slack,
    /// The caret stands inside what was typed: recorded mid-echo.
    Inside,
    /// The caret sits exactly where the typed characters put it.
    Exact,
}

/// One whole-word match on a row.
#[derive(Clone, Copy)]
struct Hit {
    /// Lead columns of the matched scalars, ascending.
    cols: [u16; MAX_CHARS],
    anchor: Anchor,
}

/// The best whole-word occurrence of `seq` in `cells` within reach of the caret:
/// by [`Anchor`], and among equals the rightmost (the word just typed is the
/// last thing on its line). Continuation halves of wide glyphs carry no scalar
/// and are skipped; columns come from the cells.
///
/// Reach is the typed-edit witness's span reach widened by `2·back_chars + 2`
/// columns, since a held address word sits a few typed characters away, each at
/// most two cells. Symmetric: a word inside one IME commit lies right of the
/// recorded caret.
///
/// Exact means the distance is what `back_chars` narrow or wide characters make
/// it: `caret == end + 1 + w·back` (typed, word on the left) or
/// `caret + w·back == start` (one commit, word on the right), `w ∈ {1, 2}`.
/// Inside means the caret stands in the span the typed characters cover (the
/// word widened by `2·back` columns each side): `sit sit␣` in one burst with
/// the caret noted at `$ s` is the first copy, not the rightmost. Slack is the
/// rest of the reach, a burst that outran its echo; that echo leaves the live
/// cursor on this row past the word by at most the typed tail (`back` and the
/// boundary key, two cells each), so Slack needs `cursor_col` there. Output
/// usually meets the reach alone.
///
/// `exact_only` (the upward search) takes the left reading alone: it follows a
/// line that moved, so it may only find a word already echoed in full (a caret
/// one past its end proves that). Under the right reading none of the word has
/// echoed, so an empty row means "not yet" and a search would find only history
/// (last time's `$ sit` starts at the same prompt column). IME commits never
/// carry an Enter, so they never need the search.
fn find_in_row(
    cells: &[RenderCell],
    seq: &[char],
    caret_col: u16,
    back_chars: u16,
    cursor_col: Option<u16>,
    exact_only: bool,
) -> Option<Hit> {
    let (first, caret, back) = (*seq.first()?, i64::from(caret_col), i64::from(back_chars));
    let reach = i64::from(TYPED_EDIT_REACH_COLS) + 2 * back + 2;
    let cursor = cursor_col.map(i64::from);
    let max_start =
        usize::try_from(caret + reach).map_or(0, |m| m.saturating_add(1).min(cells.len()));
    let mut best: Option<Hit> = None;
    for start in 0..max_start {
        if cells[start].wide || cells[start].ch != first {
            continue;
        }
        // Left bound: the previous lead cell (step over a continuation half).
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
        let anchor = if typed_left || committed_right {
            Anchor::Exact
        } else if (start - 2 * back..=end + 1 + 2 * back).contains(&caret) {
            Anchor::Inside
        } else if cursor.is_some_and(|at| (end + 1..=end + 3 + 2 * back).contains(&at)) {
            Anchor::Slack
        } else {
            continue;
        };
        // Left-to-right walk: `>=` on the rank keeps the rightmost of equals.
        if best.is_none_or(|b| anchor >= b.anchor) {
            best = Some(Hit { cols, anchor });
        }
    }
    best
}

/// [`find_in_row`] for the typed spelling and, for an RTL word, its mirror (the
/// GUI delivers an RTL run reversed in place, same columns, reversed scalars).
/// Returns the hit and whether it was mirrored.
fn find_word(
    slot: &Slot,
    cells: &[RenderCell],
    cursor_col: Option<u16>,
    exact_only: bool,
) -> Option<(Hit, bool)> {
    let n = usize::from(slot.len);
    let typed = find_in_row(
        cells,
        &slot.chars[..n],
        slot.caret.1,
        slot.back_chars,
        cursor_col,
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
        cursor_col,
        exact_only,
    );
    match (typed, mirrored) {
        (Some(a), Some(b)) if b.anchor > a.anchor => Some((b, true)),
        (Some(a), _) => Some((a, false)),
        (None, Some(b)) => Some((b, true)),
        (None, None) => None,
    }
}

/// `row − (base_y_now − base_y_then)`: where a live row stands after the scroll
/// the snapshot reports. With either side unknown the delta is zero.
fn scrolled(row: u16, then: Option<i64>, now: Option<i64>) -> (i64, i64) {
    let delta = match (then, now) {
        (Some(then), Some(now)) => now.saturating_sub(then),
        _ => 0,
    };
    (i64::from(row).saturating_sub(delta), delta)
}

/// Find a pending slot's word. Only the expected row is tested; the bounded
/// nearest-first upward search (exact anchors only) runs only when the snapshot
/// reported no scroll, the expected row came up empty, and the live `cursor`
/// says the line was left behind (a scroll region moved it).
///
/// An empty expected row also happens while the echo is late, when a search
/// could only find something else. An echo only carries the cursor rightward
/// along the caret's row, so a cursor still there at or past the recorded caret
/// means the line is still being typed; off that row, or left of the caret (a
/// fresh prompt), it was left behind. A new prompt that grew past the old caret
/// reads as still typing and costs that one flash. The same cursor on the
/// expected row is the evidence an [`Anchor::Slack`] copy needs.
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
        let cursor_col = (usize::from(cursor.0) == row).then_some(cursor.1);
        found = find_word(slot, row_buf, cursor_col, false).map(|hit| (row, hit));
        let left_behind = cursor.0 != slot.caret.0 || cursor.1 < slot.caret.1;
        if found.is_none() && delta == 0 && left_behind {
            for above in (row.saturating_sub(SEARCH_UP_ROWS)..row).rev() {
                load_row(above, row_buf);
                if let Some(hit) = find_word(slot, row_buf, None, true) {
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
        bg: [0; 3],
        sat: 0.0,
        val: 0.0,
        strength: 0.0,
    };
    // An unpaintable capture leaves `strength` at zero; the caller drops it.
    let _ = capture(&mut loc, usize::from(slot.len), row_buf);
    Some(loc)
}

/// Re-verify a located flash on a later rescan and follow it by the reported
/// scroll. `None` ends the flash (width changed, row scrolled off, text gone).
/// No second search: the previous `$ sit` two rows up has the same characters
/// at the same columns.
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
    // The shell may have recoloured the word (a syntax highlighter painting an
    // unknown command red): recapture so the fade lands on the current colour.
    capture(&mut loc, n, row_buf).then_some(loc)
}

/// Capture what a frame needs from the located row: each lead cell's resolved fg
/// and, from the first lead cell, the background the legibility guard judges
/// against. `false` when no tint would be legible (see [`legible_strength`]),
/// so there is no flash.
fn capture(loc: &mut Located, n: usize, cells: &[RenderCell]) -> bool {
    let Some(first) = cells.get(usize::from(loc.cols[0])) else {
        return false;
    };
    let same_lead = loc.strength > 0.0 && first.fg == loc.fg[0] && first.bg == loc.bg;
    for i in 0..n {
        if let Some(cell) = cells.get(usize::from(loc.cols[i])) {
            loc.fg[i] = cell.fg;
        }
    }
    if same_lead {
        return true;
    }
    loc.bg = first.bg;
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

        /// The host's fire mapping: the listener fired on `word` at a boundary
        /// key, so the note lands before that key's echo is scanned.
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

    /// On both rescan variants: a word typed to the pet flashes on its own cells,
    /// every animating frame fingerprints differently, and past the window there
    /// are zero cells, a fingerprint that stays 0, and nothing holding the
    /// scheduler awake.
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

    /// The two rescan twins are one routine outside row sourcing: the same
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

    /// The base is the current colour: when a syntax highlighter repaints the
    /// word mid-flash, later rescans recapture, so frames mix from and the fade
    /// lands on the new colour.
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

    /// Typed, never seen: `echo sit`, its output, and a committed edit keystroke
    /// beside it mint nothing; only the input path's note does.
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

    /// The row rule: `sit⏎` on the bottom row scrolls before its echo is scanned,
    /// and at this prompt width the shell's `command not found: sit` puts `sit`
    /// in exactly the typed word's columns one row nearer the caret. With
    /// `base_y` declared only the typed line's row is tested.
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
            // Negative control: with no delta, the nearest exactly anchored match
            // is the shell's output at this prompt width, which is why the host
            // declares `base_y`.
            let mut blind = Glass::new(path, 4, 40).unwired();
            assert_eq!(
                cells_at(&script(&mut blind, t0)),
                [(2, 24), (2, 25), (2, 26)],
                "{path:?}: non-vacuous — without base_y the error line wins"
            );
        }
    }

    /// A scroll region (tmux, a pager's status line) moves the typed line without
    /// moving `base_y`: the delta is zero, the caret's row is empty, and the
    /// bounded upward search takes over, exact anchors only.
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

            // A caret recorded mid-word (a burst that outran its echo) is a
            // tolerant anchor: honoured on the expected row, refused by the
            // upward search.
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

            // A held fire under the same scroll (`good kitty⏎`): exact is the
            // caret's distance in typed characters (`back_chars`), not plain
            // adjacency.
            let mut g = Glass::new(path, 6, 40);
            g.echo("\x1b[2;5r\x1b[5;1H$ good kitty", t0);
            g.fire("good", 6, "\r\nzsh: command not found: good\r\n$ ", t0);
            let flashed: Vec<(u16, u16)> = cells_at(&g.frame(t0 + ms(300)).0)
                .into_iter()
                .filter(|(_, col)| *col < 6)
                .collect();
            assert_eq!(flashed, [(2, 2), (2, 3), (2, 4), (2, 5)], "{path:?}");

            // A cursor on another row is left behind as much as one left of the
            // caret: the failing command leaves a grown prompt (`[127] $ `) on
            // the region's floor, right of the old caret's column but off its row.
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
            // Known cost: the same grown prompt on the caret's own row, at or
            // past its column, looks like an echo still arriving, and the search
            // stays home.
            let mut g = Glass::new(path, 6, 40);
            g.echo("\x1b[2;5r\x1b[5;1H$ sit", t0);
            g.fire("sit", 0, "\r\nzsh: command not found: sit\r\n[127] $ ", t0);
            assert_eq!(g.caret(), (4, 8));
            assert!(g.frame(t0 + ms(300)).0.is_empty(), "{path:?}");
        }
    }

    /// A reported scroll is the whole truth: when the snapshot says the line
    /// moved and the word is not on the row it moved to, the engine does not
    /// search. Here the typed line was wiped as it scrolled, and last time's
    /// `$ sit` stands exactly anchored a few rows up.
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
            // Negative control: the undeclared host's fallback does find it.
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

    /// Not yet is not gone: a burst commits `sit␣` before any of it echoes, so the
    /// recorded caret is the prompt's end and the first rescan sees `$ s`. The
    /// empty row there means the echo is late, and last time's `$ sit` two rows
    /// up starts at exactly that caret. The upward search must not take it,
    /// declared `base_y` or not.
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
                // A program parks the cursor elsewhere while the echo is in flight:
                // the cursor has left the caret's row, so the search may run, but it
                // still refuses the never-seen reading.
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

                // One letter further on, the left reading lines up by coincidence
                // (caret column 3, `echo sit`'s output ends at column 2), but the
                // live cursor has only moved right: the line is still echoing.
                let mut g = Glass::new(path, 8, 40);
                g.wired = wired;
                g.echo("$ echo sit\r\nsit\r\n$ s", t0);
                assert_eq!(g.caret(), (2, 3));
                g.wd.note_trick_typed(t0, None, "sit", 0);
                // A repaint elsewhere (a status clock, cursor saved and restored)
                // before any more echo: the cursor has not moved, still not "left
                // behind".
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

    /// A declaration is spent by the rescan it was made for. An undeclared rescan
    /// reads as "no row origin", never the last declared one, or its caret would
    /// pair with a pre-scroll `base_y` and the next declared rescan would test
    /// last time's `$ sit`.
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

    /// Preference on the caret's row: Exact, then Inside, then Slack, which
    /// counts only with the live cursor just past the word; among equals the
    /// rightmost wins, and out of reach is no hit.
    #[test]
    fn an_exact_anchor_beats_an_inside_one_beats_slack_and_among_equals_the_rightmost_wins() {
        use Anchor::{Exact, Inside, Slack};
        let mut g = Glass::new(Path::Cold, 2, 40);
        g.feed("$ sit sit");
        let row = g.row(0);
        let find = |caret_col: u16, cursor_col: Option<u16>| {
            find_in_row(&row, &['s', 'i', 't'], caret_col, 0, cursor_col, false)
                .map(|hit| (hit.cols[0], hit.anchor))
        };
        assert_eq!(
            find(5, None),
            Some((2, Exact)),
            "exact on the left, though the right one is within reach"
        );
        assert_eq!(find(9, None), Some((6, Exact)));
        assert_eq!(
            find(6, None),
            Some((6, Exact)),
            "the caret at the word's START"
        );
        // `sit sit␣` in one burst, the caret noted at `$ s`: the copy it stands
        // inside fired; the rightmost is only reached.
        assert_eq!(find(3, Some(10)), Some((2, Inside)), "inside beats slack");
        assert_eq!(find(8, None), Some((6, Inside)), "and the other way round");
        assert_eq!(find(20, Some(10)), None, "out of reach");

        // Slack alone: `$ ` noted, `x sit` typed in a burst.
        let mut g = Glass::new(Path::Cold, 2, 40);
        g.feed("$ x sit");
        let row = g.row(0);
        let find = |cursor_col: Option<u16>| {
            find_in_row(&row, &['s', 'i', 't'], 2, 0, cursor_col, false)
                .map(|hit| (hit.cols[0], hit.anchor))
        };
        assert_eq!(find(Some(7)), Some((4, Slack)), "the burst's own echo");
        assert_eq!(find(Some(9)), Some((4, Slack)), "and a wide boundary key");
        assert_eq!(find(Some(10)), None, "the cursor ran on past the tail");
        assert_eq!(find(Some(6)), None, "the cursor has not passed the word");
        assert_eq!(find(None), None, "the cursor is on another row");
    }

    /// `sit sit␣` typed in one burst, the caret noted at `$ s`: the phrase law
    /// fired on the first copy and absorbed the second, so the first flashes.
    #[test]
    fn a_burst_flashes_the_copy_that_fired_not_the_rightmost() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 4, 40);
            g.echo("$ s", t0);
            g.wd.note_trick_typed(t0, None, "sit", 0);
            g.echo("it sit ", t0);
            assert_eq!(
                cells_at(&g.frame(t0 + ms(300)).0),
                [(0, 2), (0, 3), (0, 4)],
                "{path:?}"
            );
        }
    }

    /// Under an unreported scroll, bash's `bash: sit: command not found` lands
    /// on the caret's row with its `sit` within reach of a two-column prompt's
    /// caret, and the cursor at the end of that line. It never flashes; the
    /// typed line is found upward once the next prompt leaves it behind.
    #[test]
    fn output_the_caret_only_reaches_never_flashes_under_an_unreported_scroll() {
        for path in BOTH {
            let t0 = Instant::now();
            let mut g = Glass::new(path, 6, 40).unwired();
            g.echo("\x1b[2;5r\x1b[5;1H$ sit", t0);
            g.fire("sit", 0, "\r\nbash: sit: command not found", t0);
            assert_eq!(g.caret().0, 4, "the error is on the caret's row");
            assert!(g.frame(t0 + ms(300)).0.is_empty(), "{path:?}: output");
            g.echo("\r\n$ ", t0 + ms(320));
            assert_eq!(
                cells_at(&g.frame(t0 + ms(600)).0),
                [(2, 2), (2, 3), (2, 4)],
                "{path:?}: the typed line"
            );
        }
    }

    /// A held address word sits a few typed characters left of the caret:
    /// `good kitty␣` flashes `good`, found through `back_chars`; without it the
    /// note is out of reach and unpainted.
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

    /// The split host ticks one engine once per pane and translates each pane's
    /// ink by its origin. Pane B holds the same text at the same cells, so an
    /// ungated flash would paint it too.
    #[test]
    fn pane_b_never_shows_pane_as_flash() {
        let t0 = Instant::now();
        let (lex, cfg) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
        let mut a = Terminal::new(4, 40);
        let mut b = Terminal::new(4, 40);
        a.process(b"$ sit");
        b.process(b"$ sit");
        // A third pane holds the word two columns along, within reach of pane A's
        // caret, so an ungated rescan would find it there and kill pane A's flash
        // at its next rescan.
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
        // must come from pane 1's parked slot.
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
        // The wake rides pane A's own parked deadline and dies with it.
        wd.bind_pane(2, (400, 0));
        assert!(wd.is_active(t0 + ms(316)));
        assert!(!wd.is_active(t0 + ms(u64::from(FLASH_MS))));
    }

    /// The flash is not parked, so the engine's `retain_panes` must reach it: a
    /// flash naming a pane that left the layout goes with it; one naming a pane
    /// that stayed is untouched.
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
            // Closing the bound pane 2 left the engine unbound, so the split
            // host's next bind is a pane-space reset; pane 1's flash survives it.
            wd.bind_pane(1, (0, 0));
            assert_eq!(wd.trick_flash_phase(t0), left, "pane {closed}: re-bound");
            if closed == 2 {
                wd.rescan(&a, 4, 40, &lex, &cfg, 3, t0);
                let (mut out, mut ink, mut free, mut nova) = (vec![], vec![], vec![], vec![]);
                let geom = EffectGeom::default();
                let t = t0 + ms(300);
                wd.tick(
                    t, &cfg, geom, None, None, true, &mut out, &mut ink, &mut free, &mut nova,
                );
                assert_eq!(cells_at(&ink), [(0, 2), (0, 3), (0, 4)], "and it paints");
            }
        }
    }

    /// A flash that names no pane is the window-wide grid's own, and the bind
    /// that makes the window a split retires it with that grid. So does a word
    /// the unsplit host noted for its session and nobody has found yet: its caret
    /// was recorded in the geometry the split just changed.
    #[test]
    fn a_flash_that_names_no_pane_dies_with_the_window_wide_grid() {
        let t0 = Instant::now();
        let (lex, cfg) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
        let mut a = Terminal::new(4, 40);
        a.process(b"$ sit");
        let mut wd = WordDecorations::default();
        wd.rescan(&a, 4, 40, &lex, &cfg, 1, t0);
        wd.note_trick_typed(t0, None, "sit", 0);
        a.process(b" ");
        wd.rescan(&a, 4, 40, &lex, &cfg, 2, t0);
        assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Live);
        wd.bind_pane(1, (0, 0));
        assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Idle);

        let mut a = Terminal::new(4, 40);
        a.process(b"$ sit");
        let mut wd = WordDecorations::default();
        wd.set_scan_session(Some(1));
        wd.rescan(&a, 4, 40, &lex, &cfg, 1, t0);
        wd.note_trick_typed(t0, Some(1), "sit", 0);
        assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
        wd.bind_pane(1, (0, 0));
        assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Idle, "unfound");
    }

    /// A host naming no pane in the note but declaring its scan session: the row
    /// origin was recorded under that session, and the note reads it from there.
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

    /// An echo landing inside the give-up budget is found, one landing after it
    /// is not, and the phase reads idle the moment the budget lapses.
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

    /// The envelope runs from the rescan that found the word, not from the key:
    /// a late echo still ramps in from the base instead of popping.
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
        // A rewound rescan clock (the capture path) never predates the key.
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("$ sit", t0);
        g.wd.note_trick_typed(t0 + ms(50), None, "sit", 0);
        g.echo(" ", t0);
        assert_eq!(g.frame(t0 + ms(50)).0[0].color, own, "since = the key time");
    }

    /// `sit tight`: the line turned out to be prose. A flash on the glass leaves
    /// over [`REVOKE_FADE_MS`] from where it stood; one never found is dropped.
    /// Revoking is idempotent.
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

        // Revoked before the echo: never painted at all.
        let mut g = Glass::new(Path::Cold, 4, 40);
        g.echo("$ sit", t0);
        g.wd.note_trick_typed(t0, None, "sit", 0);
        g.wd.revoke_trick_flash(t0 + ms(5));
        assert_eq!(g.wd.trick_flash_phase(t0 + ms(5)), TrickFlashPhase::Idle);
        g.echo(" ", t0 + ms(10));
        assert!(g.frame(t0 + ms(300)).0.is_empty());
    }

    /// Reduced motion never arms a transient tint, and ink off means no ink for
    /// anyone: no flash, nothing armed, no slot waiting, before the echo or
    /// mid-flash.
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

                // The gate closing mid-flash (a reload, a window losing focus).
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

    /// The grid holds base scalars only: Thai `นั่ง` and Hindi `बैठ` are typed
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

    /// The GUI lays an RTL run out in visual order (reversed in place) while the
    /// typed token is logical: both spellings are tried for an RTL word; an LTR
    /// word is never mirrored.
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

    /// Cyrillic is East-Asian-ambiguous: under `ambiguous_width_double` each
    /// letter is two cells, so columns come from the matched cells, not a width
    /// computed from the typed word.
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

    /// One IME commit: `ねこ、おすわり！` arrives whole and fires at `！` before any
    /// echo, so the recorded caret is where the commit starts and the word lies
    /// to its right, three wide characters along.
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

    /// Whole word: `sit` inside `site` or `visit` is someone's prose.
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

    /// `meow` is a trick word and a feline word, so the flash lands on cells the
    /// occurrence already inks with its self-glow. The merge replaces (one cell
    /// per `(row, col)`, still sorted) and mixes from the glow's colour.
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

    /// Flash cells are inserted in `(row, col)` order between other rows' ink.
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

    /// A flash that only just started is not cut off (the new word is skipped);
    /// past its opening third of a second the new word replaces it. Cosmetic;
    /// the safety is in the const asserts.
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

    /// A located flash follows its line by the reported scroll, and with no
    /// report it ends rather than search again (the shell's error line holds the
    /// same characters one row away).
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
        assert_eq!(flash.base_y_slice().len(), BASE_Y_MEMO_CAP);
        assert_eq!(
            flash.base_y_slice()[0],
            (Some(24), 24),
            "oldest-first eviction"
        );
        // A cursor-less rescan is not an observation of the caret's row origin.
        scan(&mut flash, 39, Some(1000), None);
        assert!(flash.base_y_slice().contains(&(Some(39), 39)));
        // Re-declaring updates in place; withdrawing removes.
        scan(&mut flash, 39, Some(41), Some((0, 0)));
        assert!(flash.base_y_slice().contains(&(Some(39), 41)));
        scan(&mut flash, 39, None, Some((0, 0)));
        assert!(
            flash
                .base_y_slice()
                .iter()
                .all(|(pane, _)| *pane != Some(39))
        );
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
        assert!(
            flash
                .base_y_slice()
                .iter()
                .all(|(pane, _)| *pane != Some(31))
        );

        flash.note(t0, Some(38), Some(38), Some((0, 0)), "sit", 0);
        assert_eq!(flash.slot.map(|slot| slot.base_y), Some(Some(38)));
        flash.retain_panes(|pane| pane < 30);
        assert!(
            flash
                .base_y_slice()
                .iter()
                .all(|(pane, _)| pane.is_some_and(|p| p < 30))
        );
        assert!(!flash.base_y_slice().is_empty());
        assert!(
            flash.slot.is_none(),
            "pane 38 is gone, and its flash with it"
        );
    }

    /// The legibility guard holds at every hue, not only the six it samples; it
    /// binds where it should (pure blue on dark, pure yellow on light); and text
    /// the theme draws below the bound is not tinted, ending the flash.
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
            (0x0060_6060, 0x0000_0000), // dim comment grey: below the bound
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
        // …and on the glass that is no flash, so concealed text is never
        // revealed.
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

    /// Photosensitivity measured off the real colour law (the declared rate is
    /// const-asserted): sample one cell's relative luminance at 1 kHz across the
    /// flash, over every opening hue, word position, theme and strength, and
    /// count large direction reversals (WCAG 2.3.1's unit: an opposing change of
    /// at least 10% of the maximum relative luminance).
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
            // The guarded strength the emitter would use and the unguarded peak:
            // the bound must not depend on the guard having bound.
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
