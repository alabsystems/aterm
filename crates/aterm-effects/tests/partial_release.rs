// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A RELEASE OF PART OF A RUN TAKES EXACTLY THAT PART** (2026-09-16).
//!
//! The content witness RELEASES a cell whose glyph went and stands nowhere
//! else — the run's text is gone. Until this round `Ribbon::release_cells`
//! answered any release, whole or partial, by abandoning the WHOLE cohort:
//! one trailing cell blanked under a program's own repaint drained every
//! letter still standing on the row (the Codex replay's 33 → 9 collapse
//! under a typing hand). The first cut of the fix made a partial release a
//! no-op on the ribbon, and the adversarial review of that cut measured what
//! that costs, at this seam: light standing over a program-cleared suffix
//! AHEAD of a typing hand for as long as it typed, and a later whole clear no
//! longer honoured, because the witness had marked the part released and
//! never named it again.
//!
//! The law that holds: a partial release leaves the way an erase's suffix
//! leaves — the named cells stamped onto the retract, counted, the cohort's
//! clock untouched — and stays RESTORABLE inside the melt, so a composer
//! that blanks a tail on one frame and puts the same text back on the next
//! keeps its light. A release that names every standing cell is the text
//! gone whole, and the cohort leaves as it always did.
//!
//! Driven at the host seam through a real [`Terminal`], as
//! `tests/new_line_fade.rs` drives it.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::ribbon::{RETRACT_DUR_S, RETRACT_FADE_S};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;
const CW: usize = 8;
const CH: usize = 16;
/// The whole exit: retract plus fade, with a frame of slack.
const RETRACT_SPAN_MS: u64 = ((RETRACT_DUR_S + RETRACT_FADE_S) * 1000.0) as u64 + 40;
/// A stamped cell's melt, with a frame of slack.
const FADE_MS: u64 = (RETRACT_FADE_S * 1000.0) as u64 + 40;

fn geom() -> Geom {
    Geom {
        cw: CW,
        ch: CH,
        rows: ROWS,
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * CW) as u16,
        win_h: (ROWS * CH) as u16,
        head: 0,
    }
}

fn cfg() -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style: GlowStyle::RainbowKitty,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
    }
}

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
            cfg: cfg(),
            g: geom(),
            now: Instant::now(),
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
        };
        h.frame();
        h
    }

    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            self.glow.observe_ribbon_row(r, &self.row_buf);
        }
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

/// A program clears the SUFFIX of a typed line and parks the caret back:
/// the suffix leaves as an erase's would — stamped, counted — the prefix
/// stands, and the cohort is not abandoned. RED before this round on
/// `retired` (the first cut counted nothing) and on the cohort (the shipped
/// law abandoned it).
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
        h.cells(5).len(),
        3,
        "past the melt the suffix is out of the pool"
    );
    assert!(
        h.lit_cols(5).iter().all(|&c| c < 3),
        "no light over the cleared cells: {:?}",
        h.lit_cols(5)
    );
    // A glyph landing later under the still-standing prefix is replaced text
    // as before — and counted once more, for a different cell.
    h.program(b"\x1b[6;1HXXX\x1b[6;4H");
    assert_eq!(h.leaving(5), vec![0, 1, 2]);
    assert_eq!(h.retired(), 11);
}

/// A whole clear AFTER a partial one is still the text gone whole: the
/// cohort is abandoned into its retract and the row is dark inside the
/// retract's span. RED on the first cut: the held records of the earlier
/// release were never named again, the release was never "whole", and the
/// band left on the cells' own life ~1.3 s later.
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

/// The Ink dance on a PART of the line: the suffix blanked on one frame,
/// the same text put back on the next. The stamps are lifted, the cells
/// live again, nothing was abandoned. RED on the first cut (no clock was
/// saved, so the restore was refused and the records stayed released
/// under text that was back).
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
    // The eleven cells, plus the ATTACH the head carries one cell on under
    // the caret at 11.
    let lit = h.lit_cols(5);
    assert!(
        (0..11).all(|c| lit.contains(&c)) && lit.iter().all(|&c| c <= 11),
        "and the light stands whole: {lit:?}"
    );
    // A whole clear after the restored partial still leaves through the swoosh.
    h.program(b"\x1b[6;1H\x1b[2K\x1b[6;1H");
    assert_eq!(
        h.cohorts(5),
        vec![(true, false)],
        "the text gone whole abandons the cohort: {:?} / cells {:?}",
        h.cohorts(5),
        h.cells(5)
    );
    // Each cell once: the eight the partial counted came BACK and are not
    // counted again when they leave with the whole; the whole adds the
    // three letters and the space the partial never named.
    assert_eq!(
        h.retired(),
        12,
        "8 by the partial, 4 more by the whole, each cell once"
    );
}

/// A suffix the program cleared leaves no light AHEAD of a hand that keeps
/// typing: the stamped cells melt, the new keys lay their own. RED on the
/// first cut: `lit == 0..=10` with the caret at 9 for as long as the hand
/// typed.
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
