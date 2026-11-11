// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The letter under the caret reads.
//!
//! A program that moves the caret onto a letter (vim's `k`, an arrow key
//! into a typed line) puts the caret's body on that letter. A block caret
//! draws the letter cut out of its fill in the cell's own ground, so the
//! letter reads exactly the fill's contrast with that ground; the letter
//! guard leaves the caret's cell to the caret, so nothing else kept it.
//! The trail gallery's `move-up` scene (looks8 WIDEN) read the `!` the caret
//! landed on at 1.00:1 under Laser, 2.45:1 under Water and 4.05:1 under
//! Fire on the dark page. Three laws keep it:
//!
//! - Laser's bolt stands down to a steady block of its own colour over a
//!   letter ([`CursorGlow::caret_bolt_stands_down`]): a bolt is no block, so
//!   the renderer cuts no letter out of it, and drawn over the letter it hid
//!   it.
//! - An effect's block fill over a letter keeps [`caret_letter_min_contrast`]
//!   against the ground under it, not the page floor alone
//!   ([`crate::cursor_fx::floor_block_fill`]): the cut-out's solid ink then
//!   reads the letter guard's floor.
//! - No over-text light lies on the caret's letter
//!   ([`CursorGlow::keep_light_off_caret_letter`]). The block hides that
//!   light in the frame, but the post passes drawn over the finished frame
//!   (the SDR crown screens each over-text quad onto it, the bloom blurs
//!   them) put it back on the cut-out: Water's bead, over the caret's cell,
//!   lifted the `!` from the page's `#111318` to `#1c4f60`. With the caret
//!   blinked off, or a bar, the letter is drawn plain and that light lay on
//!   it unguarded.

use super::bed::punch_quads;
use super::{CursorGlow, Geom, GlowQuad, GlowStyle, InkLedger};
use aterm_render::GlowRamp;

/// The share of its block's contrast a letter cut out of a block caret
/// reads, with room: the median of its core against the median of its
/// ground, which binds before its solid ink's reading (the letter guard's
/// model) on a thin glyph. The gallery's `move-up`
/// read its `!` at 4.05:1 cut out of Fire's 4.81:1 ember (0.84), and the
/// replay at 4.52:1 out of a 5.41:1 fill (0.835).
const CUT_OUT_SHARE: f32 = 0.80;

/// The least contrast an effect-owned block fill keeps against the ground
/// under it when the caret's cell holds a letter: the letter is cut out of
/// the fill in that ground, so it reads [`CUT_OUT_SHARE`] of the fill's
/// contrast, and keeps the letter guard's floor ([`InkLedger::LETTER_FLOOR`]).
#[inline]
#[must_use]
pub(crate) const fn caret_letter_min_contrast() -> f32 {
    InkLedger::LETTER_FLOOR / CUT_OUT_SHARE
}

/// One law that keeps the letter under the caret, for a test's negative
/// control ([`CursorGlow::set_caret_letter_law`]).
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaretLetterLaw {
    /// Laser's bolt stands down to a block over a letter.
    BoltStandsDown,
    /// An effect's block fill over a letter keeps the letter's floor.
    LetterFloor,
    /// No over-text light lies on the caret's letter.
    LightOff,
}

/// The laws a test turned off ([`CaretLetterLaw`]).
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LawsOff {
    bolt: bool,
    floor: bool,
    light: bool,
}

impl CursorGlow {
    /// A test's negative control: the caret-letter law `law` on (`true`, as
    /// shipped) or off.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_caret_letter_law(&mut self, law: CaretLetterLaw, on: bool) {
        match law {
            CaretLetterLaw::BoltStandsDown => self.caret_letter_off.bolt = !on,
            CaretLetterLaw::LetterFloor => self.caret_letter_off.floor = !on,
            CaretLetterLaw::LightOff => self.caret_letter_off.light = !on,
        }
    }

    /// Whether the caret at `cur` sits on a letter whose law `off` names
    /// is still on.
    #[inline]
    const fn caret_letter_kept(&self, cur: Option<(u16, u16)>, off: bool) -> bool {
        match cur {
            Some(cell) => !off && self.caret_lettered(cell),
            None => false,
        }
    }

    /// Whether Laser's bolt stands down to a block this frame: the caret at
    /// `cur` sits on a letter.
    #[inline]
    #[must_use]
    pub(crate) const fn caret_bolt_stands_down(&self, cur: Option<(u16, u16)>) -> bool {
        #[cfg(any(test, debug_assertions))]
        let off = self.caret_letter_off.bolt;
        #[cfg(not(any(test, debug_assertions)))]
        let off = false;
        self.caret_letter_kept(cur, off)
    }

    /// Whether the caret's block fill keeps the letter's floor this frame
    /// ([`caret_letter_min_contrast`]): the caret at `cur` sits on a letter.
    #[inline]
    #[must_use]
    pub(crate) const fn caret_fill_keeps_letter(&self, cur: Option<(u16, u16)>) -> bool {
        #[cfg(any(test, debug_assertions))]
        let off = self.caret_letter_off.floor;
        #[cfg(not(any(test, debug_assertions)))]
        let off = false;
        self.caret_letter_kept(cur, off)
    }

    /// Punch the caret's cell out of the over-text stream `over` when the
    /// caret at `cur` sits on a letter, under every `style` but Classic's
    /// spectrum face on a dark theme, whose ruled v0.28 bytes the punch
    /// leaves as the letter guard does ([`Self::classic_keeps_its_bytes`]).
    /// Returns a print of the cut (0: none).
    #[inline]
    pub(crate) fn keep_light_off_caret_letter(
        &mut self,
        over: &mut Vec<GlowQuad>,
        cur: Option<(u16, u16)>,
        geom: Geom,
        style: GlowStyle,
    ) -> u64 {
        #[cfg(any(test, debug_assertions))]
        let off = self.caret_letter_off.light;
        #[cfg(not(any(test, debug_assertions)))]
        let off = false;
        if self.classic_keeps_its_bytes(style) {
            return 0;
        }
        let Some((row, col)) = cur else {
            return 0;
        };
        if off || !self.caret_lettered((row, col)) {
            return 0;
        }
        if geom.cw == 0 || geom.ch == 0 {
            return 0;
        }
        let hole = Self::caret_box(geom, row, col);
        // A ramp the cell lies in is laid as the quads it stands for, which
        // are punched; the stream's other ramps keep their places among its
        // quads ([`punch_quads`]).
        let items = self.out_ramps.items.as_slice();
        let mut any_ramp = false;
        let mut ri = 0;
        while ri < items.len() {
            if ramp_meets(&items[ri], hole) {
                any_ramp = true;
                break;
            }
            ri += 1;
        }
        if any_ramp {
            self.out_ramps.flatten_into(over);
        }
        let cut = punch_quads(
            over,
            &mut self.out_ramps.items,
            &mut self.caret_punch_scratch,
            hole,
        );
        if cut == 0 {
            return 0;
        }
        (u64::from(row) << 48) | (u64::from(col) << 32) | cut as u64
    }
}

/// Whether any of `ramp`'s rows meets the box `(x0, y0, x1, y1)`.
#[inline]
const fn ramp_meets(ramp: &GlowRamp, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> bool {
    let (rx0, ry0) = (ramp.x as i32, ramp.y as i32);
    ramp.w > 0
        && ramp.h > 0
        && rx0 < x1
        && rx0 + ramp.w as i32 > x0
        && ry0 < y1
        && ry0 + ramp.h as i32 > y0
}

#[cfg(test)]
#[path = "caret_letter_tests.rs"]
mod tests;
