// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Transient rainbow flash over a typed pet-trick word ([`crate::typed_tricks`]), located on glass via [`Anchor`].

use std::time::Duration;

use aterm_core::terminal::RenderCell;
use aterm_hash::{FNV64_OFFSET, fnv_mix_u64};
use aterm_lexicon::{TrickLexicon, is_no_space_script, is_token_char};
use aterm_render::InkCell;
use aterm_scene::smoothstep;
use aterm_time::Instant;

use crate::color_math::hsv2rgb;
#[cfg(test)]
use crate::color_math::relative_luminance;
use crate::effect_util::{TWINKLE_FLASH_BOUND_HZ, lerp_rgb as mix_rgb};
use crate::spec::{MIN_ANIMATION_MS, RAINBOW_SAT, RAINBOW_SPAN_DEG, RAINBOW_VAL};
use crate::word_decorations::{
    INK_FADE_MS, INK_RAMP_IN_MS, MIN_INK_CONTRAST, TYPED_EDIT_REACH_COLS, TYPED_EDIT_WITNESS_MS,
    fold_ink, rainbow_base_hue, rgb3_to_u32, u32_to_rgb3,
};

/// The whole flash in ms (ramp in, hold, fade out), also the hue cycle's period.
pub(crate) const FLASH_MS: u32 = 1100;
const RAMP_MS: u32 = 130;
const FADE_MS: u32 = 250;
const REVOKE_FADE_MS: u32 = 120;
const GIVE_UP_MS: u64 = TYPED_EDIT_WITNESS_MS;
const SEARCH_UP_ROWS: usize = 12;
const FLASH_STRENGTH: f32 = 0.9;
const MAX_CHARS: usize = TrickLexicon::MAX_SURFACE_CHARS;
const BASE_Y_MEMO_CAP: usize = 16;

const FLASH_REVERSALS: f32 = 3.0;
const FLASH_REVERSAL_HZ: f32 = FLASH_REVERSALS * 1000.0 / FLASH_MS as f32;

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

/// Where the flash slot stands at `now`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrickFlashPhase {
    /// No flash: never noted, finished, given up, dropped, or revoked and gone.
    Idle,
    /// Noted by the input path and not yet found on the glass.
    Pending,
    /// Found; the envelope is running.
    Live,
    /// Found and then revoked.
    Revoked,
}

/// One noted word: what was typed, where the hands were, and — once a rescan
/// has found it — where it is.
#[derive(Clone, Copy, Debug)]
struct Slot {
    /// When the boundary key was committed (host input time).
    at: Instant,
    /// The pane the key went to.
    pane: Option<u64>,
    /// The pre-echo caret of that pane, `(row, col)`.
    caret: (u16, u16),
    /// That pane's `base_y` at the scan which recorded the caret, when the host declares one.
    base_y: Option<i64>,
    /// The typed word, zero-width scalars removed, in logical order.
    chars: [char; MAX_CHARS],
    len: u8,
    /// Typed characters between the word and the caret (0 for a plain fire).
    back_chars: u16,
    /// The word holds a strong-RTL scalar.
    rtl: bool,
    /// Decides the flash's opening hue.
    hue_seed: u64,
    located: Option<Located>,
    /// Set once by a revoke on a located slot.
    revoked_at: Option<Instant>,
}

/// Where a slot's word was found, and everything the emitter needs so that a
/// frame reads no grid at all.
#[derive(Clone, Copy, Debug)]
struct Located {
    /// The envelope's zero: `max(rescan clock, key time)`.
    since: Instant,
    row: u16,
    base_y: Option<i64>,
    /// The grid width it was found at.
    grid_cols: u16,
    /// The match was the mirrored (visual-order) spelling.
    mirrored: bool,
    /// Lead cells only, ascending.
    cols: [u16; MAX_CHARS],
    fg: [[u8; 3]; MAX_CHARS],
    bg: [u8; 3],
    /// Resolved for the word's background (light backgrounds take deep tones).
    sat: f32,
    val: f32,
    /// The legibility-guarded peak tint, computed at locate time rather than per frame.
    strength: f32,
}

/// The flash state a word engine owns.
#[derive(Debug)]
pub(crate) struct TrickFlash {
    slot: Option<Slot>,
    /// When the last flash became visible.
    last_start: Option<Instant>,
    /// What the host declared for its next rescan, and for which scan scope.
    declared: Option<(Option<u64>, i64)>,
    /// Per-pane `base_y` as of that pane's last cursor-bearing rescan.
    base_y: [(Option<u64>, i64); BASE_Y_MEMO_CAP],
    base_y_len: u8,
}

impl Default for TrickFlash {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl TrickFlash {
    #[inline]
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            slot: None,
            last_start: None,
            declared: None,
            base_y: [(None, 0); BASE_Y_MEMO_CAP],
            base_y_len: 0,
        }
    }

    #[cfg(test)]
    #[inline]
    const fn base_y_slice(&self) -> &[(Option<u64>, i64)] {
        self.base_y.split_at(self.base_y_len as usize).0
    }

    #[inline]
    const fn lookup_base_y(&self, target: Option<u64>) -> Option<i64> {
        let len = self.base_y_len as usize;
        let mut idx = 0;
        while idx < len {
            if matches!((self.base_y[idx].0, target), (None, None))
                || matches!((self.base_y[idx].0, target), (Some(a), Some(b)) if a == b)
            {
                return Some(self.base_y[idx].1);
            }
            idx += 1;
        }
        None
    }

    /// The host's declaration for the one rescan it is about to run in `scope`, which spends it.
    #[inline]
    pub(crate) const fn declare_base_y(&mut self, scope: Option<u64>, base_y: Option<i64>) {
        self.declared = match base_y {
            Some(y) => Some((scope, y)),
            None => None,
        };
    }

    /// Forget panes the window no longer shows.
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
    #[inline]
    pub(crate) fn retain_panes(&mut self, keep: impl Fn(u64) -> bool) {
        let mut w = 0usize;
        let len = usize::from(self.base_y_len);
        let mut r = 0usize;
        while r < len {
            let entry = self.base_y[r];
            r += 1;
            let keep_entry = match entry.0 {
                None => true,
                Some(pane) => keep(pane),
            };
            if keep_entry {
                self.base_y[w] = entry;
                w += 1;
            }
        }
        self.base_y_len = w as u8;
        if let Some((Some(pane), _)) = self.declared
            && !keep(pane)
        {
            self.declared = None;
        }
        if let Some(slot) = &self.slot
            && let Some(pane) = slot.pane
            && !keep(pane)
        {
            self.slot = None;
        }
    }

    /// Whether the slot names the pane its key went to and was found there.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickFlashLifecycle",
            action = "Rebind",
            project = "conformance_trick_flash::project"
        )
    )]
    #[inline]
    pub(crate) const fn found_in_a_pane(&self) -> bool {
        matches!(
            &self.slot,
            Some(Slot {
                pane: Some(_),
                located: Some(_),
                ..
            })
        )
    }

    /// Drop the flash.
    #[inline]
    pub(crate) const fn clear(&mut self) {
        self.slot = None;
    }

    /// The input path says `word` was just typed to the pet in `pane`.
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
    #[inline]
    pub(crate) fn note(
        &mut self,
        now: Instant,
        pane: Option<u64>,
        scope: Option<u64>,
        caret: Option<(u16, u16)>,
        word: &str,
        back_chars: u16,
    ) {
        let Some(caret) = caret else {
            return;
        };
        const MIN_ANIMATION_DUR: Duration = Duration::from_millis(MIN_ANIMATION_MS as u64);
        if let Some(started) = self.last_start
            && now.saturating_duration_since(started) < MIN_ANIMATION_DUR
        {
            return;
        }
        let mut chars = ['\0'; MAX_CHARS];
        let mut len = 0usize;
        let mut rtl = false;
        for c in word.chars() {
            // The grid keeps a zero-width scalar on the previous cell, so it can
            // never be found as a cell of its own.
            let zero_width = if c.is_ascii() {
                c.is_ascii_control()
            } else {
                aterm_grapheme::char_width(c) == 0
            };
            if zero_width {
                continue;
            }
            if len == MAX_CHARS {
                return;
            }
            chars[len] = c;
            len += 1;
            rtl |= !c.is_ascii() && is_strong_rtl(c);
        }
        if len == 0 {
            return;
        }
        let base_y = self.lookup_base_y(match pane {
            Some(p) => Some(p),
            None => scope,
        });
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

    /// The line was not pet talk.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickFlashLifecycle",
            action = "Revoke",
            project = "conformance_trick_flash::project"
        )
    )]
    #[inline]
    pub(crate) fn revoke(&mut self, now: Instant) {
        let Some(slot) = self.slot.as_mut() else {
            return;
        };
        match &slot.located {
            None => self.slot = None,
            Some(loc) => {
                if slot.revoked_at.is_none() {
                    slot.revoked_at = Some(now.max(loc.since));
                }
            }
        }
    }

    /// See [`TrickFlashPhase`].
    #[inline]
    pub(crate) fn phase(&self, now: Instant) -> TrickFlashPhase {
        let Some(slot) = self.slot.as_ref() else {
            return TrickFlashPhase::Idle;
        };
        match &slot.located {
            None if millis_since(now, slot.at) > GIVE_UP_MS => TrickFlashPhase::Idle,
            None => TrickFlashPhase::Pending,
            Some(loc) if millis_since(now, loc.since) >= window_ms(loc, slot.revoked_at) => {
                TrickFlashPhase::Idle
            }
            Some(_) if slot.revoked_at.is_some() => TrickFlashPhase::Revoked,
            Some(_) => TrickFlashPhase::Live,
        }
    }

    /// The rescan half, called by both rescan variants after their scan pass.
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
    #[inline]
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
        let base_y_now = match self.declared.take() {
            Some((declared_for, y)) if declared_for == scope => Some(y),
            _ => None,
        };
        if cursor.is_some() {
            self.remember_base_y(scope, base_y_now);
        }
        let Some(mut slot) = self.slot else {
            return;
        };
        if !pane_matches(slot.pane, scope) {
            return;
        }
        let (true, Some(cursor)) = (paintable, cursor) else {
            self.slot = None;
            return;
        };
        let grid_cols = if cols < usize::from(u16::MAX) {
            cols as u16
        } else {
            u16::MAX
        };
        self.slot = match slot.located {
            Some(loc) => match follow(
                &slot,
                loc,
                base_y_now,
                rows,
                grid_cols,
                row_buf,
                &mut load_row,
            ) {
                Some(loc) => {
                    slot.located = Some(loc);
                    Some(slot)
                }
                None => None,
            },
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
                Some(_) => None,
                None => Some(slot),
            },
        };
    }

    /// The tick half: merge this frame's flash cells into `ink`.
    #[inline]
    pub(crate) fn emit(
        &mut self,
        scope: Option<u64>,
        now: Instant,
        paintable: bool,
        ink: &mut Vec<InkCell>,
        fp: &mut u64,
    ) -> Option<Instant> {
        let slot = self.slot.as_ref()?;
        if !pane_matches(slot.pane, scope) {
            return None;
        }
        if !paintable {
            self.slot = None;
            return None;
        }
        let loc = slot.located.as_ref()?;
        let t = millis_since(now, loc.since);
        let window = window_ms(loc, slot.revoked_at);
        if t >= window {
            self.slot = None;
            return None;
        }
        let revoked_t = slot.revoked_at.map(|at| millis_since(at, loc.since));
        let tint = envelope(t, revoked_t) * loc.strength;
        let n = usize::from(slot.len);
        let has_tint = tint > 0.0;
        let full_tint = tint >= 1.0;
        let (base_hue, span_per_step) = if has_tint {
            const DEG_PER_MS: f32 = 360.0 / FLASH_MS as f32;
            let hue0 = rainbow_base_hue(slot.hue_seed);
            let steps = n.saturating_sub(1) as f32;
            let span = RAINBOW_SPAN_DEG.min(100.0 * steps);
            let span_per_step = span / steps.max(1.0);
            let time_deg = t as f32 * DEG_PER_MS;
            (hue0 + time_deg, span_per_step)
        } else {
            (0.0, 0.0)
        };
        let loc_row = loc.row;
        let loc_sat = loc.sat;
        let loc_val = loc.val;
        let loc_since = loc.since;
        let mut search_from = 0usize;
        let mut i = 0;
        while i < n {
            let target = if has_tint {
                let hue = base_hue + i as f32 * span_per_step;
                hsv2rgb(hue, loc_sat, loc_val)
            } else {
                0
            };
            let col = loc.cols[i];
            let fg_i = loc.fg[i];
            i += 1;
            let cell = if !matches!(ink.last(), Some(c) if (c.row, c.col) >= (loc_row, col)) {
                let color = if full_tint {
                    u32_to_rgb3(target)
                } else if has_tint {
                    u32_to_rgb3(mix_rgb(rgb3_to_u32(fg_i), target, tint))
                } else {
                    fg_i
                };
                let cell = InkCell {
                    row: loc_row,
                    col,
                    color,
                };
                ink.push(cell);
                search_from = ink.len();
                cell
            } else {
                let key = (loc_row, col);
                let lookup = match ink.get(search_from) {
                    Some(c) if (c.row, c.col) == key => Ok(0),
                    Some(c) if (c.row, c.col) > key => Err(0),
                    _ => find_ink_cell(&ink[search_from..], key),
                };
                match lookup {
                    Ok(rel) => {
                        let j = search_from + rel;
                        search_from = j + 1;
                        if full_tint {
                            ink[j].color = u32_to_rgb3(target);
                        } else if has_tint {
                            ink[j].color =
                                u32_to_rgb3(mix_rgb(rgb3_to_u32(ink[j].color), target, tint));
                        }
                        ink[j]
                    }
                    Err(rel) => {
                        let j = search_from + rel;
                        search_from = j + 1;
                        let color = if full_tint {
                            u32_to_rgb3(target)
                        } else if has_tint {
                            u32_to_rgb3(mix_rgb(rgb3_to_u32(fg_i), target, tint))
                        } else {
                            fg_i
                        };
                        let cell = InkCell {
                            row: loc_row,
                            col,
                            color,
                        };
                        ink.insert(j, cell);
                        cell
                    }
                }
            };
            *fp = fold_ink(*fp, cell);
        }
        *fp = fnv_mix_u64(*fp, t);
        Some(loc_since + Duration::from_millis(window))
    }

    #[inline]
    const fn remember_base_y(&mut self, scope: Option<u64>, base_y: Option<i64>) {
        let len = self.base_y_len as usize;
        let mut at = None;
        let mut idx = 0;
        while idx < len {
            if matches!((self.base_y[idx].0, scope), (None, None))
                || matches!((self.base_y[idx].0, scope), (Some(a), Some(b)) if a == b)
            {
                at = Some(idx);
                break;
            }
            idx += 1;
        }
        match (at, base_y) {
            (Some(i), Some(y)) => self.base_y[i].1 = y,
            (Some(i), None) => {
                let mut k = i;
                while k + 1 < len {
                    self.base_y[k] = self.base_y[k + 1];
                    k += 1;
                }
                self.base_y_len -= 1;
            }
            (None, Some(y)) => {
                if len >= BASE_Y_MEMO_CAP {
                    let mut k = 0;
                    while k + 1 < BASE_Y_MEMO_CAP {
                        self.base_y[k] = self.base_y[k + 1];
                        k += 1;
                    }
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
#[inline]
const fn hue_seed(caret: (u16, u16), chars: &[char]) -> u64 {
    let mut h = fnv_mix_u64(fnv_mix_u64(FNV64_OFFSET, caret.0 as u64), caret.1 as u64);
    let mut i = 0;
    while i < chars.len() {
        h = fnv_mix_u64(h, chars[i] as u64);
        i += 1;
    }
    h
}

/// The pane test every hook shares (and the typed-edit witness's): `None` on
/// either side names no pane (a single-grid host, or an engine with no declared
/// session), so there is nothing to cross; two named panes must be equal.
#[inline]
const fn pane_matches(flash_pane: Option<u64>, scope: Option<u64>) -> bool {
    match (flash_pane, scope) {
        (Some(typed_in), Some(scanning)) => typed_in == scanning,
        _ => true,
    }
}

/// `now − then` in ms, zero when `now` is the earlier one: the introspection
/// capture path can legitimately hand the engine a rewound clock.
#[inline]
fn millis_since(now: Instant, then: Instant) -> u64 {
    now.saturating_duration_since(then).as_millis() as u64
}

/// How long a located flash lasts, ms from `since`: the whole window, or —
/// revoked — the short fade from the revoke, whichever ends first.
#[inline]
fn window_ms(loc: &Located, revoked_at: Option<Instant>) -> u64 {
    let whole = FLASH_MS as u64;
    match revoked_at {
        Some(at) => {
            let faded = millis_since(at, loc.since) + REVOKE_FADE_MS as u64;
            if faded < whole { faded } else { whole }
        }
        None => whole,
    }
}

/// The tint envelope at `t` ms: smooth ramp in, hold, smoothstep fade over the
/// last [`FADE_MS`]; zero at both ends.
#[inline]
const fn envelope(t: u64, revoked_t: Option<u64>) -> f32 {
    const INV_RAMP_MS: f32 = 1.0 / RAMP_MS as f32;
    const INV_FADE_MS: f32 = 1.0 / FADE_MS as f32;
    const INV_REVOKE_FADE_MS: f32 = 1.0 / REVOKE_FADE_MS as f32;
    let tf = t as f32;
    let ramp = if t >= RAMP_MS as u64 {
        1.0
    } else {
        smoothstep(tf * INV_RAMP_MS)
    };
    let fade = if t <= (FLASH_MS - FADE_MS) as u64 {
        1.0
    } else {
        1.0 - smoothstep((tf - (FLASH_MS - FADE_MS) as f32) * INV_FADE_MS)
    };
    let revoke = match revoked_t {
        Some(r) if t > r => 1.0 - smoothstep((tf - r as f32) * INV_REVOKE_FADE_MS),
        _ => 1.0,
    };
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

#[inline]
const fn resolve_sat_val_from_lum(lum_bg: f32) -> (f32, f32) {
    if lum_bg > 0.5 {
        (1.0, 0.62)
    } else {
        (RAINBOW_SAT, RAINBOW_VAL)
    }
}

/// The rainbow colorway's theme rule: a light background takes deep candy
/// tones that can clear the contrast bound on white.
#[cfg(test)]
#[inline]
const fn resolve_sat_val(bg: u32) -> (f32, f32) {
    let table = crate::color_math::relative_luminance_table();
    resolve_sat_val_from_lum(crate::color_math::relative_luminance_with(table, bg))
}

/// The legibility guard: the strongest tint, stepping down from
/// [`FLASH_STRENGTH`] in eighths, at which the flash is legible at every hue it
/// passes through.
#[cfg(test)]
#[inline]
const fn legible_strength(fg: u32, bg: u32, sat: f32, val: f32) -> f32 {
    let table = crate::color_math::relative_luminance_table();
    let lum_bg = crate::color_math::relative_luminance_with(table, bg);
    legible_strength_with(table, fg, lum_bg, sat, val)
}

#[inline]
const fn legible_strength_with(
    table: &crate::color_math::RelativeLuminanceTable,
    fg: u32,
    lum_bg: f32,
    sat: f32,
    val: f32,
) -> f32 {
    let floor = MIN_INK_CONTRAST;
    let text_is_lighter = crate::color_math::relative_luminance_with(table, fg) >= lum_bg;
    let bg_plus = lum_bg + 0.05;
    let c = val * sat;
    let m = val - c;
    let hi = ((c + m).clamp(0.0, 1.0) * 255.0).round() as u32 as f32;
    let lo = (m.clamp(0.0, 1.0) * 255.0).round() as u32 as f32;
    let (fr, fg_c, fb) = (
        ((fg >> 16) & 0xff) as f32,
        ((fg >> 8) & 0xff) as f32,
        (fg & 0xff) as f32,
    );
    #[inline]
    const fn mix_byte(ca: f32, cb: f32, t: f32) -> usize {
        (((ca + (cb - ca) * t) + 0.5) as u32 & 0xff) as usize
    }
    let step = FLASH_STRENGTH * 0.125;
    let mut strength = FLASH_STRENGTH;
    'guard: while strength > 0.0 {
        let t = strength.clamp(0.0, 1.0);
        let (lr_hi, lr_lo) = (
            table.red[mix_byte(fr, hi, t)],
            table.red[mix_byte(fr, lo, t)],
        );
        let (lg_hi, lg_lo) = (
            table.green[mix_byte(fg_c, hi, t)],
            table.green[mix_byte(fg_c, lo, t)],
        );
        let (lb_hi, lb_lo) = (
            table.blue[mix_byte(fb, hi, t)],
            table.blue[mix_byte(fb, lo, t)],
        );
        let c0 = (lr_hi + lg_lo) + lb_lo;
        let c1 = (lr_hi + lg_hi) + lb_lo;
        let c2 = (lr_lo + lg_hi) + lb_lo;
        let c3 = (lr_lo + lg_hi) + lb_hi;
        let c4 = (lr_lo + lg_lo) + lb_hi;
        let c5 = (lr_hi + lg_lo) + lb_hi;
        let fails = if text_is_lighter {
            let min_lum = c0.min(c1).min(c2).min(c3).min(c4).min(c5);
            min_lum < lum_bg || (min_lum + 0.05) / bg_plus < floor
        } else {
            let max_lum = c0.max(c1).max(c2).max(c3).max(c4).max(c5);
            max_lum >= lum_bg || bg_plus / (max_lum + 0.05) < floor
        };
        if fails {
            strength = (strength - step).max(0.0);
            continue 'guard;
        }
        break;
    }
    strength
}

/// A scalar inside a word on the glass: a token character of a spaced script,
/// or any no-space-script scalar.
#[inline]
fn is_word_char(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric() || c == '_'
    } else {
        is_token_char(c) || is_no_space_script(c)
    }
}

/// Strong right-to-left scalars: Hebrew, Arabic and neighbours in the BMP, the
/// presentation forms, and the SMP's RTL planes.
#[inline]
const fn is_strong_rtl(c: char) -> bool {
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
    /// Within reach and nothing more.
    Slack,
    /// The caret stands inside what was typed.
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

#[inline]
fn find_in_row(
    cells: &[RenderCell],
    seq: &[char],
    caret_col: u16,
    back_chars: u16,
    cursor_col: Option<u16>,
    exact_only: bool,
) -> Option<Hit> {
    let (first, caret, back) = (*seq.first()?, i64::from(caret_col), i64::from(back_chars));
    let two_back = 2 * back;
    let (caret_plus_back, caret_plus_two_back) = (caret + back, caret + two_back);
    let three_plus_two_back = 3 + two_back;
    let reach = i64::from(TYPED_EDIT_REACH_COLS) + two_back + 2;
    let max_start = if caret + reach >= 0 {
        let max_possible = (cells.len() + 1).saturating_sub(seq.len());
        ((caret + reach) as usize)
            .saturating_add(1)
            .min(max_possible)
    } else {
        0
    };
    let mut best: Option<Hit> = None;
    let mut start = 0;
    while start < max_start {
        let cur_start = start;
        start += 1;
        if cells[cur_start].wide || cells[cur_start].ch != first {
            continue;
        }
        let mut prev = cur_start;
        let mut word_before = false;
        while prev > 0 {
            prev -= 1;
            if !cells[prev].wide {
                word_before = is_word_char(cells[prev].ch);
                break;
            }
        }
        if word_before {
            continue;
        }
        let mut cols = [0u16; MAX_CHARS];
        let mut at = cur_start;
        let mut matched = true;
        let mut k = 0;
        while k < seq.len() {
            let want = seq[k];
            while at < cells.len() && cells[at].wide {
                at += 1;
            }
            if at < cells.len() && at <= u16::MAX as usize && cells[at].ch == want {
                cols[k] = at as u16;
            } else {
                matched = false;
                break;
            }
            at += 1;
            k += 1;
        }
        if !matched {
            continue;
        }
        while at < cells.len() && cells[at].wide {
            at += 1;
        }
        if at < cells.len() && is_word_char(cells[at].ch) {
            continue;
        }
        let (start, end) = (cur_start as i64, at as i64 - 1);
        let end_1 = end + 1;
        if caret < start - reach || caret > end_1 + reach {
            continue;
        }
        let end_1_two_back = end_1 + two_back;
        let typed_left = caret == end_1 + back || caret == end_1_two_back;
        let committed_right = caret_plus_back == start || caret_plus_two_back == start;
        if exact_only && !typed_left {
            continue;
        }
        let anchor = if typed_left || committed_right {
            Anchor::Exact
        } else if (start - two_back..=end_1_two_back).contains(&caret) {
            Anchor::Inside
        } else if matches!(cursor_col, Some(at) if (end_1..=end + three_plus_two_back).contains(&i64::from(at)))
        {
            Anchor::Slack
        } else {
            continue;
        };
        if !matches!(best, Some(b) if anchor < b.anchor) {
            best = Some(Hit { cols, anchor });
        }
    }
    best
}

/// [`find_in_row`] for the typed spelling and, for an RTL word, its mirror (the
/// GUI delivers an RTL run reversed in place, same columns, reversed scalars).
#[inline]
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
    if !slot.rtl || matches!(typed, Some(h) if h.anchor == Anchor::Exact) {
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

#[inline]
const fn find_ink_cell(slice: &[InkCell], (row, col): (u16, u16)) -> Result<usize, usize> {
    let mut lo = 0usize;
    let mut hi = slice.len();
    while lo < hi {
        let mid = lo + ((hi - lo) >> 1);
        let c = &slice[mid];
        if c.row < row || (c.row == row && c.col < col) {
            lo = mid + 1;
        } else if c.row > row || c.col > col {
            hi = mid;
        } else {
            return Ok(mid);
        }
    }
    Err(lo)
}

/// `row − (base_y_now − base_y_then)`: where a live row stands after the scroll
/// the snapshot reports.
#[inline]
const fn scrolled(row: u16, then: Option<i64>, now: Option<i64>) -> (i64, i64) {
    let delta = match (then, now) {
        (Some(then), Some(now)) => now.saturating_sub(then),
        _ => 0,
    };
    ((row as i64).saturating_sub(delta), delta)
}

/// Find a pending slot's word.
#[allow(
    clippy::too_many_arguments,
    reason = "on_rescan's list minus the gate it already applied; the slot, the clock, both carets, the geometry and the row source are each read once"
)]
#[inline]
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
    if expected >= 0 && (expected as usize) < rows {
        let row = expected as usize;
        load_row(row, row_buf);
        let cursor_col = if usize::from(cursor.0) == row {
            Some(cursor.1)
        } else {
            None
        };
        found = find_word(slot, row_buf, cursor_col, false).map(|hit| (row, hit));
        let left_behind = cursor.0 != slot.caret.0 || cursor.1 < slot.caret.1;
        if found.is_none() && delta == 0 && left_behind {
            let min_above = row.saturating_sub(SEARCH_UP_ROWS);
            let mut above = row;
            while above > min_above {
                above -= 1;
                load_row(above, row_buf);
                if let Some(hit) = find_word(slot, row_buf, None, true) {
                    found = Some((above, hit));
                    break;
                }
            }
        }
    }
    let (row, (hit, mirrored)) = found?;
    if row > u16::MAX as usize {
        return None;
    }
    let mut loc = Located {
        since: now.max(slot.at),
        row: row as u16,
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
    let _ = capture(&mut loc, usize::from(slot.len), row_buf);
    Some(loc)
}

#[inline]
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
    let (row_i64, _) = scrolled(loc.row, loc.base_y, base_y_now);
    if row_i64 < 0 {
        return None;
    }
    let row = row_i64 as usize;
    if row >= rows || row > u16::MAX as usize {
        return None;
    }
    load_row(row, row_buf);
    let n = usize::from(slot.len);
    let mut i = 0;
    while i < n {
        let want = slot.chars[if loc.mirrored { n - 1 - i } else { i }];
        let col_idx = usize::from(loc.cols[i]);
        if col_idx >= row_buf.len() || row_buf[col_idx].wide || row_buf[col_idx].ch != want {
            return None;
        }
        i += 1;
    }
    loc.row = row as u16;
    loc.base_y = base_y_now;
    if capture(&mut loc, n, row_buf) {
        Some(loc)
    } else {
        None
    }
}

/// Capture what a frame needs from the located row: each lead cell's resolved fg
/// and, from the first lead cell, the background the legibility guard judges
/// against.
#[inline]
const fn capture(loc: &mut Located, n: usize, cells: &[RenderCell]) -> bool {
    let first_col = loc.cols[0] as usize;
    if first_col >= cells.len() {
        return false;
    }
    let first = &cells[first_col];
    let same_lead = loc.strength > 0.0
        && first.fg[0] == loc.fg[0][0]
        && first.fg[1] == loc.fg[0][1]
        && first.fg[2] == loc.fg[0][2]
        && first.bg[0] == loc.bg[0]
        && first.bg[1] == loc.bg[1]
        && first.bg[2] == loc.bg[2];
    let mut i = 0;
    while i < n {
        let col = loc.cols[i] as usize;
        if col < cells.len() {
            loc.fg[i] = cells[col].fg;
        }
        i += 1;
    }
    if same_lead {
        return true;
    }
    loc.bg = first.bg;
    let table = crate::color_math::relative_luminance_table();
    let lum_bg = crate::color_math::relative_luminance_rgb(
        table,
        first.bg[0] as usize,
        first.bg[1] as usize,
        first.bg[2] as usize,
    );
    (loc.sat, loc.val) = resolve_sat_val_from_lum(lum_bg);
    loc.strength = legible_strength_with(table, rgb3_to_u32(first.fg), lum_bg, loc.sat, loc.val);
    loc.strength > 0.0
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
#[path = "trick_flash_tests.rs"]
mod tests;
