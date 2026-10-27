// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pipeline's debug frame reads for `lab trail-diff` (`docs/design/PRISM-GLASS.md` §15.1
//! P0.3): every trail engine's state words, the frame's light report, the bed's ink table and
//! the glow's next change. Debug and test builds only.

use super::EffectsPipeline;
use crate::rainbow_kitty::ribbon::BedInkLut;
use crate::state_words::{ArmsDrawn, StateWords, Words, glow_cfg_words};
use aterm_time::{Duration, Instant};

impl EffectsPipeline {
    /// Every trail engine's state words this frame, each time relative to `t0`: the resolved
    /// glow config, the glow (its custody, ink ledger and v2 engine), the rainbow caret, the pet
    /// driver and its brain, then Sparkle Words.
    #[inline]
    #[must_use]
    pub fn state_words(&self, t0: Instant) -> StateWords {
        let mut w = StateWords::new(t0);
        w.group("pipeline.glow_cfg", |f| glow_cfg_words(f, &self.glow_cfg));
        self.cursor.glow.state_words(&mut w);
        self.cursor.rainbow.state_words(&mut w);
        self.companion.state_words(&mut w);
        self.decos.state_words(&mut w);
        w
    }

    /// Which light arms drew this frame and the streams each touched (§15.0.2(b)). No light arm
    /// exists yet, so no frame reports one.
    #[inline]
    #[must_use]
    pub const fn light_arms_drawn(&self) -> ArmsDrawn {
        ArmsDrawn::NONE
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_words::Word;
    use aterm_core::terminal::Terminal;

    /// The owner's rainbow kitty with its pet, typing `text` at 100 ms a key into a fresh
    /// terminal, then resting 400 ms, its clock started at `t0`: the pipeline at the end, and
    /// its words after every frame, read against `read_t0`.
    fn typed(t0: Instant, read_t0: Instant, text: &[u8]) -> (EffectsPipeline, Vec<Vec<Word>>) {
        let mut p = EffectsPipeline::new();
        p.t0 = t0;
        p.set_cursor_glow(
            true,
            "rainbow kitty",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            0.9,
            0.8,
            true,
            0x0050_FA7B,
        );
        p.set_cursor_pet(true, 0x5EED);
        let mut term = Terminal::new(6, 30);
        term.process(b"\x1b[3;2H");
        let mut input = term.cell_frame(6, 30);
        let mut frames = Vec::new();
        p.advance(16.0);
        p.apply(&mut term, &mut input, 10, 19);
        frames.push(p.state_words(read_t0).into_words());
        for &b in text {
            assert!(p.note_committed_char(&mut term, &input, char::from(b)));
            term.process(&[b]);
            p.advance(100.0);
            term.cell_frame_into(&mut input, 6, 30);
            p.apply(&mut term, &mut input, 10, 19);
            frames.push(p.state_words(read_t0).into_words());
        }
        for _ in 0..25 {
            p.advance(16.0);
            term.cell_frame_into(&mut input, 6, 30);
            p.apply(&mut term, &mut input, 10, 19);
            frames.push(p.state_words(read_t0).into_words());
        }
        (p, frames)
    }

    #[test]
    fn state_words_do_not_move_with_t0() {
        let early = Instant::now();
        let late = early + Duration::from_secs(5);
        let (_, a) = typed(early, early, b"the quick fox");
        let (_, b) = typed(late, late, b"the quick fox");
        let word =
            |frame: &[Word], name: &str| frame.iter().find(|&&(n, _)| n == name).map(|&(_, v)| v);
        assert!(
            a.iter().any(|f| word(f, "rk.engaged") == Some(1)),
            "fixture: the v2 engine owns the trail"
        );
        for (fixture, name) in [
            ("the typing laid a ribbon", "ribbon.cells.n"),
            ("the sky minted stars", "stardust.minted"),
        ] {
            assert!(
                a.iter().any(|f| word(f, name).is_some_and(|n| n > 0)),
                "fixture: {fixture}"
            );
        }
        assert_eq!(a.len(), b.len());
        for (i, (wa, wb)) in a.iter().zip(&b).enumerate() {
            assert_eq!(
                wa, wb,
                "frame {i}: the same script started 5 s later writes the same words"
            );
        }
        let (_, misread) = typed(late, early, b"the quick fox");
        assert_ne!(
            a, misread,
            "the negative control: read against another start, the script's clocks move"
        );
        let (_, dog) = typed(early, early, b"the quick dog");
        assert_ne!(a, dog, "the words read what was typed");
    }

    #[test]
    fn no_light_arm_draws_before_phase_one() {
        let t0 = Instant::now();
        let (p, _) = typed(t0, t0, b"lit");
        assert!(p.light_arms_drawn().is_empty());
        assert!(p.light_arms_drawn().streams().is_empty());
    }

    #[test]
    fn the_bed_ink_lut_read_is_the_table_the_ribbon_synced() {
        let t0 = Instant::now();
        let (p, _) = typed(t0, t0, b"lit");
        let lut = p.bed_ink_lut();
        let (fg, bg, dark, _tall) = lut.key().expect("the typed ribbon synced its table");
        assert_eq!(
            (fg, bg, dark),
            (
                p.glow_cfg.theme_fg,
                p.glow_cfg.theme_bg,
                p.glow_cfg.dark_theme
            )
        );
        assert!(lut.lut().iter().any(|&ink| ink != 0), "the table holds ink");
    }
}
