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

    /// A named change to a fresh pipeline's carry-over, given the words' `t0`.
    type Case = (&'static str, fn(&mut EffectsPipeline, Instant));

    fn carried(p: &EffectsPipeline, t0: Instant) -> Vec<Word> {
        p.state_words(t0)
            .into_words()
            .into_iter()
            .filter(|&(n, _)| n.starts_with("pipeline."))
            .collect()
    }

    fn geometry() -> CursorCoordinateSpace {
        CursorCoordinateSpace {
            rows: 24,
            cols: 80,
            cell_w: 9,
            cell_h: 18,
            chrome_pad: 0,
            chrome_head: 0,
        }
    }

    #[test]
    fn each_carried_cursor_input_moves_the_pipeline_words() {
        let t0 = Instant::now();
        let base = carried(&EffectsPipeline::new(), t0);
        assert_eq!(base, carried(&EffectsPipeline::new(), t0));
        let cases: [Case; 7] = [
            ("the typing cadence", |p, t0| {
                p.cursor.cadence.on_keystroke(t0)
            }),
            ("the keystrokes owed", |p, _| p.pending_keys = 1),
            ("the flying head's sky row", |p, _| {
                p.flying_sky_row = Some(3)
            }),
            ("the scroll snapshot", |p, _| {
                p.cursor_scroll_state = Some(ContentScrollState::default());
            }),
            ("the geometry", |p, _| {
                p.cursor_coordinate_space = Some(geometry());
            }),
            ("the screen", |p, _| p.cursor_owner_space = Some((7, false))),
            ("the line's ownership", |p, _| p.program_owns_line = true),
        ];
        for (what, set) in cases {
            let mut p = EffectsPipeline::new();
            set(&mut p, t0);
            assert_ne!(carried(&p, t0), base, "{what} is hashed");
        }

        let with = |scroll: ContentScrollState, space: CursorCoordinateSpace| {
            let mut p = EffectsPipeline::new();
            p.cursor_scroll_state = Some(scroll);
            p.cursor_coordinate_space = Some(space);
            carried(&p, t0)
        };
        let plain = with(ContentScrollState::default(), geometry());
        let mut moved = ContentScrollState::default();
        moved.bands[2].delta = -1;
        assert_ne!(with(moved, geometry()), plain, "a band's move is hashed");
        let headed = CursorCoordinateSpace {
            chrome_head: 1,
            ..geometry()
        };
        assert_ne!(
            with(ContentScrollState::default(), headed),
            plain,
            "the chrome above the grid is hashed"
        );
    }

    #[test]
    fn the_screen_is_hashed_by_its_kind_not_its_terminal() {
        let t0 = Instant::now();
        let on = |space| {
            let mut p = EffectsPipeline::new();
            p.cursor_owner_space = Some(space);
            carried(&p, t0)
        };
        assert_eq!(
            on((7, false)),
            on((8, false)),
            "a terminal's id counts the process's terminals"
        );
        assert_ne!(
            on((7, false)),
            on((7, true)),
            "the alternate screen is hashed"
        );
    }

    #[test]
    fn the_ledger_words_are_the_ink_ledgers_products_and_not_state() {
        let t0 = Instant::now();
        let (p, _) = typed(t0, t0, b"the quick fox");
        let ledger = p.ledger_words(t0);
        let names: Vec<&str> = ledger.words().iter().map(|&(n, _)| n).collect();
        assert_eq!(
            names,
            ["ink.rails", "ink.rail_ramps", "ink.dims", "ink.notes"],
            "the ledger's four products"
        );
        let state = p.state_words(t0);
        assert!(
            state.get("ink.on").is_some() && state.get("ink.pair").is_some(),
            "the account stays state"
        );
        for &(name, _) in ledger.words() {
            assert_eq!(state.get(name), None, "{name} is not state");
        }
    }

    #[test]
    fn the_light_report_is_the_cursor_steps_and_empty_on_dark() {
        let t0 = Instant::now();
        let (p, _) = typed(t0, t0, b"lit");
        assert_eq!(p.light_arms_drawn(), p.light_arms());
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
