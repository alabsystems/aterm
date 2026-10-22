// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A release of part of a run takes exactly that part.
//!
//! The content witness releases a cell whose glyph went and stands nowhere
//! else. A partial release leaves like an erase's suffix: the named cells are
//! stamped onto the retract and counted, the cohort's clock is untouched, and
//! exact identities stay restorable, dark after the melt, until the original
//! run expires or editing revokes them. A composer that restores a delayed
//! repaint therefore keeps its light. A release naming
//! every standing cell is the text gone whole, and the cohort leaves.
//!
//! Driven at the host seam through a real [`Terminal`], as
//! `tests/new_line_fade.rs` drives it.

use crate::common::{
    caret_sample, geom, note_blink, observe_caret_row, observe_ribbon_rows, rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::ribbon::{RETRACT_DUR_S, RETRACT_FADE_S};
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;
const CW: usize = 8;
const CH: usize = 16;
/// The whole exit: retract plus fade, with a frame of slack.
const RETRACT_SPAN_MS: u64 = ((RETRACT_DUR_S + RETRACT_FADE_S) * 1000.0) as u64 + 40;
/// A stamped cell's melt, with a frame of slack.
const FADE_MS: u64 = (RETRACT_FADE_S * 1000.0) as u64 + 40;

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
}

impl Host {
    fn at_row(row: u16) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(format!("\x1b[{};1H", row + 1).as_bytes());
        let mut h = Self {
            term,
            glow: CursorGlow::default(),
            cfg: rainbow_cfg(),
            g: geom(ROWS, COLS, CW, CH),
            now: Instant::now(),
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
        };
        h.frame();
        h
    }

    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink_seen, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
    }

    fn idle(&mut self, ms: u64) {
        let end = self.now + Duration::from_millis(ms);
        while self.now < end {
            self.now += Duration::from_millis(16);
            self.frame();
        }
    }

    fn key(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(90);
        self.glow.note_typed_cells(self.now, 1);
        self.term.process(bytes);
        self.frame();
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            let mut b = [0u8; 4];
            self.key(ch.encode_utf8(&mut b).as_bytes());
        }
    }

    /// Program output with no key behind it, one frame on.
    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }

    fn cells(&self, row: u16) -> Vec<(u16, bool)> {
        let mut v: Vec<(u16, bool)> = self
            .glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cells()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| (c.col, c.leaving()))
            .collect();
        v.sort_unstable();
        v
    }

    fn live(&self, row: u16) -> Vec<u16> {
        self.cells(row)
            .into_iter()
            .filter(|&(_, l)| !l)
            .map(|(c, _)| c)
            .collect()
    }

    fn leaving(&self, row: u16) -> Vec<u16> {
        self.cells(row)
            .into_iter()
            .filter(|&(_, l)| l)
            .map(|(c, _)| c)
            .collect()
    }

    /// `(abandoned, retracting)` per cohort on the row.
    fn cohorts(&self, row: u16) -> Vec<(bool, bool)> {
        self.glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cohorts()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| (c.abandoned, c.phase.is_retracting()))
            .collect()
    }

    fn row_quads(&self, row: u16) -> impl Iterator<Item = &GlowQuad> {
        let centre = f32::from(self.g.origin_y) + (f32::from(row) + 0.5) * CH as f32;
        self.glow.under_quads().iter().filter(move |q| {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            q.w > 0 && q.alpha > 0 && y0 <= centre && centre < y1
        })
    }

    /// Columns with any light (alpha > 8) at the row's centre.
    fn lit_cols(&self, row: u16) -> Vec<u16> {
        let mut cols = [false; COLS];
        for q in self.row_quads(row) {
            if q.alpha < 8 {
                continue;
            }
            let x0 = usize::from(q.x);
            let x1 = x0 + usize::from(q.w);
            let c1 = x1.div_ceil(CW).min(COLS);
            let c0 = (x0 / CW).min(c1);
            for lit in &mut cols[c0..c1] {
                *lit = true;
            }
        }
        cols.iter()
            .enumerate()
            .filter(|&(_, &l)| l)
            .map(|(i, _)| i as u16)
            .collect()
    }

    fn retired(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.retired)
    }

    /// Idle until the row is dark or `max_ms`; the ms it took.
    fn dark_after(&mut self, row: u16, max_ms: u64) -> Option<u64> {
        let start = self.now;
        while self.now.duration_since(start).as_millis() < u128::from(max_ms) {
            self.now += Duration::from_millis(16);
            self.frame();
            if self.lit_cols(row).is_empty() {
                return Some(self.now.duration_since(start).as_millis() as u64);
            }
        }
        None
    }
}

/// `hello world` typed at column 0 of `row`, the band rested 200 ms.
fn hello(row: u16) -> Host {
    let mut h = Host::at_row(row);
    h.type_str("hello world");
    assert_eq!(h.live(row), (0..11).collect::<Vec<u16>>());
    h.idle(200);
    h
}

/// A program clears the suffix of a typed line and parks the caret back: the
/// suffix leaves as an erase's would (stamped, counted), the prefix stands,
/// and the cohort is not abandoned.
#[test]
fn a_suffix_the_program_cleared_leaves_and_the_prefix_stands() {
    let mut h = hello(5);
    h.program(b"\x1b[6;4H\x1b[K\x1b[6;12H");
    assert_eq!(
        h.leaving(5),
        (3..11).collect::<Vec<u16>>(),
        "the eight cells whose text went are leaving"
    );
    assert_eq!(h.live(5), vec![0, 1, 2], "`hel` stands");
    assert_eq!(
        h.cohorts(5),
        vec![(false, false)],
        "the cohort keeps its clock"
    );
    assert_eq!(h.retired(), 8, "eight cells whose text went, counted once");
    h.idle(FADE_MS);
    assert_eq!(
        h.live(5),
        vec![0, 1, 2],
        "past the melt only the prefix owns light; the suffix keeps dark redraw custody"
    );
    assert!(
        h.lit_cols(5).iter().all(|&c| c < 3),
        "no light over the cleared cells: {:?}",
        h.lit_cols(5)
    );
    // A glyph landing under the standing prefix is replaced text, counted once more.
    h.program(b"\x1b[6;1HXXX\x1b[6;4H");
    assert!(
        h.live(5).is_empty(),
        "the replaced prefix no longer owns light"
    );
    assert_eq!(h.retired(), 11);
}

/// `hello world ` (the trailing space never armed) rests 200 ms, then the program
/// clears from `w` on: the space leaves with the cleared suffix, since no standing
/// glyph of the run lies right of it, and the cohort keeps its clock.
#[test]
fn a_trailing_space_after_a_suffix_the_program_cleared_leaves_with_it() {
    let mut h = Host::at_row(5);
    h.type_str("hello world ");
    assert_eq!(h.live(5), (0..12).collect::<Vec<u16>>());
    h.idle(200);
    h.program(b"\x1b[6;7H\x1b[K\x1b[6;13H");
    assert_eq!(
        h.leaving(5),
        (6..12).collect::<Vec<u16>>(),
        "`world` and the trailing space after it leave together"
    );
    assert_eq!(h.live(5), (0..6).collect::<Vec<u16>>(), "`hello ` stands");
    assert_eq!(
        h.cohorts(5),
        vec![(false, false)],
        "the cohort keeps its clock"
    );
    assert_eq!(h.retired(), 6, "six cells whose text went, counted once");
    h.idle(FADE_MS);
    assert_eq!(
        h.live(5),
        (0..6).collect::<Vec<u16>>(),
        "past the melt only the prefix owns light; the suffix and tail keep dark redraw custody"
    );
    assert!(
        h.lit_cols(5).iter().all(|&c| c < 6),
        "no light past `hello `: {:?}",
        h.lit_cols(5)
    );
}

/// A whole clear after a partial one is the text gone whole: the cohort is
/// abandoned into its retract and the row is dark inside the retract's span.
#[test]
fn a_whole_clear_after_a_suffix_the_program_cleared_still_leaves_through_the_swoosh() {
    let mut h = hello(5);
    h.program(b"\x1b[6;4H\x1b[K\x1b[6;12H");
    h.idle(100);
    h.program(b"\x1b[6;1H\x1b[2K\x1b[6;1H");
    assert_eq!(
        h.cohorts(5),
        vec![(true, false)],
        "the released band is abandoned into its retract: {:?}",
        h.cohorts(5)
    );
    let dark = h.dark_after(5, 1500).expect("the row goes dark");
    assert!(
        dark <= RETRACT_SPAN_MS,
        "a wholesale-cleared row leaves inside the retract, not on the cells' own life: dark after {dark} ms"
    );
    assert_eq!(
        h.retired(),
        11,
        "every cell counted once: 8 by the suffix, 3 by the whole"
    );
}

/// Ink's blank-and-restore on part of the line: the suffix blanked on one frame,
/// the same text back on the next. The stamps lift and nothing is abandoned.
#[test]
fn a_partial_clear_and_an_identical_rewrite_a_frame_later_restore_the_light() {
    let mut h = hello(5);
    h.program(b"\x1b[?25l\x1b[6;4H\x1b[K");
    assert_eq!(
        h.leaving(5),
        (3..11).collect::<Vec<u16>>(),
        "blanked: leaving"
    );
    h.program(b"\x1b[6;4Hlo world\x1b[6;12H\x1b[?25h");
    assert_eq!(
        h.live(5),
        (0..11).collect::<Vec<u16>>(),
        "the same text back inside the melt lifts the stamps: {:?} / leaving {:?}",
        h.live(5),
        h.leaving(5)
    );
    assert_eq!(h.cohorts(5), vec![(false, false)]);
    h.idle(300);
    // The eleven cells, plus the attach the head carries under the caret at 11.
    let lit = h.lit_cols(5);
    assert!(
        (0..11).all(|c| lit.contains(&c)) && lit.iter().all(|&c| c <= 11),
        "and the light stands whole: {lit:?}"
    );
    h.program(b"\x1b[6;1H\x1b[2K\x1b[6;1H");
    assert_eq!(
        h.cohorts(5),
        vec![(true, false)],
        "the text gone whole abandons the cohort: {:?} / cells {:?}",
        h.cohorts(5),
        h.cells(5)
    );
    // Each cell once: the eight the partial counted came back; the whole adds the
    // three letters and the space.
    assert_eq!(
        h.retired(),
        12,
        "8 by the partial, 4 more by the whole, each cell once"
    );
}

/// A suffix the program cleared leaves no light ahead of a hand that keeps
/// typing: the stamped cells melt, the new keys lay their own.
#[test]
fn a_suffix_the_program_cleared_leaves_no_light_ahead_of_a_typing_hand() {
    let mut h = hello(5);
    h.program(b"\x1b[6;4H\x1b[K");
    h.type_str("pppppp");
    h.idle(FADE_MS);
    let caret = h.term.cursor().col;
    assert_eq!(caret, 9);
    // The hand's cells and the attach under the caret; nothing past it.
    let lit = h.lit_cols(5);
    assert!(
        lit.iter().all(|&c| c <= caret),
        "no light past the caret over cells the program cleared: {lit:?}"
    );
    assert_eq!(
        h.live(5),
        (0..9).collect::<Vec<u16>>(),
        "the hand's own cells stand"
    );
}
