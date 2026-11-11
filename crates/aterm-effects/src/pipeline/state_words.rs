// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pipeline's debug frame reads for `lab trail-diff` (`docs/design/PRISM-GLASS.md` §15.1
//! P0.3): every trail engine's state words, the ink ledger's per-frame products (§15.9's
//! preamble), the frame's light report, the bed's ink table and the glow's next change. Debug and
//! test builds only.
//!
//! The pipeline's own words (`pipeline.*`) name what it carries from one frame to the next for
//! the cursor engines: the resolved glow config, the typing cadence, the keystrokes it owes the
//! next advance, the sky row the flying head last sat over, and the scroll snapshot, geometry,
//! screen and line ownership it last bound them to. The screen's terminal is hashed only by
//! presence: its id counts the process's terminals. Not hashed: the page's settings (the glow
//! request, its trail colour and accent, the trail config, Serious Mode, the Sparkle Words and
//! streak knobs, the chrome), the web host's injected clock, the frame's scratch and output
//! buffers, the other effects' engines (the output streak, the phosphor rain) and the window's
//! companions (typed-word detectors, sing-along notes, cameos).

use super::{CursorCoordinateSpace, EffectsPipeline};
use crate::rainbow_kitty::ribbon::BedInkLut;
use crate::state_words::{ArmsDrawn, Fold, StateWords, Words, glow_cfg_words};
use aterm_core::terminal::ContentScrollState;
use aterm_time::{Duration, Instant};

impl EffectsPipeline {
    /// Every trail engine's state words this frame, each time relative to `t0`: the pipeline's
    /// own (the resolved glow config and what it carries for the cursor engines), the glow (its
    /// custody, ink ledger and v2 engine), the rainbow caret, the pet driver and its brain, then
    /// Sparkle Words.
    #[inline]
    #[must_use]
    pub fn state_words(&self, t0: Instant) -> StateWords {
        let mut w = StateWords::new(t0);
        w.group("pipeline.glow_cfg", |f| glow_cfg_words(f, &self.glow_cfg));
        w.group("pipeline.cadence", |f| self.cursor.cadence.state_words(f));
        w.put("pipeline.pending_keys", u64::from(self.pending_keys));
        w.u16_opt("pipeline.flying_sky_row", self.flying_sky_row);
        w.opt(
            "pipeline.scroll_state",
            self.cursor_scroll_state.as_ref(),
            scroll_state_words,
        );
        w.opt(
            "pipeline.coordinate_space",
            self.cursor_coordinate_space.as_ref(),
            coordinate_space_words,
        );
        w.opt(
            "pipeline.owner_space",
            self.cursor_owner_space,
            |f, (_terminal, alt): (u64, bool)| f.flag("alt", alt),
        );
        w.flag("pipeline.program_owns_line", self.program_owns_line);
        self.cursor.glow.state_words(&mut w);
        self.cursor.rainbow.state_words(&mut w);
        self.companion.state_words(&mut w);
        self.decos.state_words(&mut w);
        w
    }

    /// The glow's ink ledger's per-frame products, each time relative to `t0`: the rails it
    /// noted off the frame's under stream, its dims and the print of its cuts. A light arm that
    /// draws in the under stream moves them, so they are not state: the dump prints them as its
    /// `ledger` line, which `lab trail-diff` compares as a stream, exempt where an arm touched
    /// `glow_under` or `glow_under_ramps` (`docs/design/PRISM-GLASS.md` §15.9's preamble).
    #[inline]
    #[must_use]
    pub fn ledger_words(&self, t0: Instant) -> StateWords {
        let mut w = StateWords::new(t0);
        self.cursor.glow.ledger_words(&mut w);
        w
    }

    /// Which light arms drew this frame and the streams each touched (§15.0.2(b)): the last
    /// cursor step's ([`EffectsPipeline::light_arms`]).
    #[inline]
    #[must_use]
    pub fn light_arms_drawn(&self) -> ArmsDrawn {
        self.light_arms()
    }

    /// The bed's ink table the trail draws by ([`BedInkLut`]).
    #[inline]
    #[must_use]
    pub const fn bed_ink_lut(&self) -> &BedInkLut {
        self.cursor.glow.bed_ink_lut()
    }

    /// The glow's next visible change after `now` at the host's `frame_interval`
    /// ([`crate::cursor_glow::CursorGlow::next_change_deadline`]), the deadline the dump reports.
    #[inline]
    #[must_use]
    pub fn glow_next_change_deadline(
        &self,
        now: Instant,
        frame_interval: Duration,
    ) -> Option<Instant> {
        self.cursor.glow.next_change_deadline(now, frame_interval)
    }
}

/// The terminal's content-scroll snapshot the cursor engines were last translated by: its
/// uniform rows, its epochs and counters, and the ring of band moves.
#[inline]
fn scroll_state_words(f: &mut Fold, s: &ContentScrollState) {
    f.put("uniform_up_rows", s.uniform_up_rows);
    f.put("invalidation_epoch", s.invalidation_epoch);
    f.put("band_batches", s.band_batches);
    f.put("band_seq", s.band_seq);
    f.list("bands", &s.bands, |f, band| {
        f.put("top", u64::from(band.top));
        f.put("bottom", u64::from(band.bottom));
        f.put("delta", band.delta as u16 as u64);
    });
    f.put("screen_clears", s.screen_clears);
}

/// The frame geometry the cursor engines were last bound to.
#[inline]
fn coordinate_space_words(f: &mut Fold, s: &CursorCoordinateSpace) {
    f.put("rows", s.rows as u64);
    f.put("cols", s.cols as u64);
    f.put("cell_w", s.cell_w as u64);
    f.put("cell_h", s.cell_h as u64);
    f.put("chrome_pad", u64::from(s.chrome_pad));
    f.put("chrome_head", u64::from(s.chrome_head));
}

#[cfg(test)]
#[path = "state_words_tests.rs"]
mod tests;
