// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing inside a lit line keeps the tail it pushes lit.
//!
//! The take: a line typed at 12 cps, the caret walked back into it with ← (stamped as the host
//! stamps a navigation key: `clear_typed_keeping_hand`, then `note_motion`), a rest, then six keys
//! typed inside the line, each an insert the program answers by rewriting the rest of the line
//! one column on. The shift pass (`Witness::shift_runs`, `Ribbon::shift_run`, run by
//! `Engine::follow_rows` before the tick) finds the tail's glyphs where the insert pushed them and
//! moves their light with them, clocks intact; a key a hidden-caret composer reports at the
//! print's end is laid at the insert it made (`Ribbon::insert_landing`).
//!
//! The shift pass (`Witness::shift_runs`, `Ribbon::shift_run`, run by
//! `Engine::follow_rows` before the tick) finds the tail's glyphs where the
//! insert pushed them and moves their light with them, clocks intact; a key a
//! hidden-caret composer reports at the print's end is laid at the insert it
//! made (`Ribbon::insert_landing`).
//!
//! The repaint arrives in six shapes ([`Shape`]): the Ink-shaped split
//! whole-line redraw, the same bytes in one read, the hide alone in the first
//! read, Claude Code's own diff frame whole or torn after its hide, and
//! readline's visible reprint-and-`CUB`. The census is the plan's coverage on
//! the composer row: `#` at ≥ 20 coverage, `.` dark.

use crate::common::{
    LIT_COV, caret_sample, class_of, coverage, geom, holes_of, note_blink, observe_caret_row,
    observe_ribbon_rows, rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, GlowConfig};
use std::time::{Duration, Instant};

const CW: usize = 8;
const CH: usize = 16;
const ROWS: usize = 60;
const COLS: usize = 140;
/// 12 cps.
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
/// The Ink-shaped composer's pause between a redraw's two reads.
const INK_SPLIT_MS: u64 = 30;
/// A hole standing longer than this many frames is the defect.
const HOLE_FRAMES_MAX: usize = 3;

/// How the composer's repaint reaches the glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// The Ink-shaped redraw: hide, home, clear, the whole line | present |
    /// 30 ms | the caret placed and shown | present.
    InkSplit,
    /// The same bytes in one read, one present.
    InkWhole,
    /// The hide alone in the first read | present | 30 ms | the line, the
    /// caret, the show | present: a frame torn before its glyphs.
    InkHideFirst,
    /// Claude Code's diff frame, the recorded shape: `?2026h ?25l`, the
    /// changed glyphs on the composer row, a CUP to the hint row, the
    /// caret's CUP, `?25h ?2026l` — one read, one present.
    ClaudeWhole,
    /// The same frame torn after `?2026h ?25l`: the hold capped, the rest
    /// a read later.
    ClaudeTorn,
    /// readline at a visible caret: the edit column on reprinted, then `CUB` back to the caret.
    Visible,
}

/// One frame's census of the composer row.
#[derive(Clone, Debug)]
struct Census {
    ms: u64,
    label: &'static str,
    text: String,
    lit: String,
    holes: Vec<u16>,
    /// Columns of the row that carry a cell of any cohort, leaving or not.
    cells: Vec<u16>,
}

/// The host: one terminal, the kitty, one clock, the composer's state.
struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    t0: Instant,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    shape: Shape,
    /// The composer's row, and the grid column of its text's first glyph.
    row: u16,
    col0: usize,
    text: String,
    caret: usize,
    label: &'static str,
    census: Vec<Census>,
}

impl Host {
    fn new(shape: Shape) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        // Ink's composer at the top of the alt screen; Claude Code's in its
        // bottom-pinned box behind `│ > `.
        let (row, col0) = match shape {
            Shape::ClaudeWhole | Shape::ClaudeTorn => ((ROWS - 3) as u16, 4),
            _ => (2, 0),
        };
        term.process(format!("\x1b[?1049h\x1b[2J\x1b[{};1H", row + 1).as_bytes());
        if col0 > 0 {
            term.process(format!("│ > \x1b[{};{}H", row + 1, col0 + 1).as_bytes());
        }
        let mut h = Self {
            term,
            glow,
            cfg: rainbow_cfg(),
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            shape,
            row,
            col0,
            text: String::new(),
            caret: 0,
            label: "start",
            census: Vec::new(),
        };
        h.frame();
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// The render path: the frame hold's caret row and every ribbon row whether or not the caret
    /// is visible, the print anchor fed just before the tick (`app_render.rs`), the tick, then
    /// the census.
    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .observe_print_anchor(aterm_effects::host::motion_print_anchor(&self.term).0);
        let g = geom(ROWS, COLS, CW, CH);
        self.glow.tick(cur, self.now, &self.cfg, g, &mut self.out);
        self.record();
    }

    fn record(&mut self) {
        let row = self.row;
        let cov = coverage(&self.glow, geom(ROWS, COLS, CW, CH), row);
        // Every dark column between the first and last lit one.
        let holes = holes_of(&cov)
            .into_iter()
            .flat_map(|(s, n)| s..s + n)
            .map(|i| i as u16)
            .collect();
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let mut cells: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| c.col)
            .collect();
        cells.sort_unstable();
        cells.dedup();
        self.term.row_cols_into(usize::from(row), &mut self.row_buf);
        let text: String = self
            .row_buf
            .iter()
            .map(|&c| if c == '\0' { '_' } else { c })
            .collect();
        let lit: String = cov
            .iter()
            .map(|&v| if v >= LIT_COV { '#' } else { '.' })
            .collect();
        self.census.push(Census {
            ms: self.ms(),
            label: self.label,
            text: text.trim_end().to_string(),
            lit: lit.trim_end_matches('.').to_string(),
            holes,
            cells,
        });
    }

    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        self.frame();
    }

    fn idle_to(&mut self, t: Instant) {
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.step();
        }
    }

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.idle_to(t);
    }

    /// The frames up to `ms` after the last event, then that instant.
    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = self.now.max(t);
        self.last_event = self.now;
    }

    /// The CUP to text column `col` of the composer.
    fn cup(&self, col: usize) -> String {
        format!("\x1b[{};{}H", self.row + 1, self.col0 + col + 1)
    }

    /// The Ink redraw's two halves: hide, home, clear, `line`; the caret
    /// placed and shown.
    fn ink_halves(&self, line: &str) -> (String, String) {
        (
            format!("\x1b[?25l\x1b[{};1H\x1b[2K{line}", self.row + 1),
            format!("{}\x1b[?25h", self.cup(self.caret)),
        )
    }

    /// The program's repaint after its text went from `before` to
    /// `self.text` (the caret at `self.caret`), in this host's shape.
    fn repaint(&mut self, before: &str) {
        match self.shape {
            Shape::InkSplit => {
                let (head, tail) = self.ink_halves(&self.text);
                self.term.process(head.as_bytes());
                self.frame();
                self.now += Duration::from_millis(INK_SPLIT_MS);
                self.term.process(tail.as_bytes());
            }
            Shape::InkWhole => {
                let (head, tail) = self.ink_halves(&self.text);
                self.term.process(format!("{head}{tail}").as_bytes());
            }
            Shape::InkHideFirst => {
                let (head, tail) = self.ink_halves(&self.text);
                self.term.process(b"\x1b[?25l");
                self.frame();
                self.now += Duration::from_millis(INK_SPLIT_MS);
                self.term.process(format!("{head}{tail}").as_bytes());
            }
            Shape::ClaudeWhole | Shape::ClaudeTorn => {
                // The diff: from the first changed text column on, the
                // rest of the row erased behind it.
                let first = before
                    .chars()
                    .zip(self.text.chars())
                    .position(|(a, b)| a != b)
                    .unwrap_or_else(|| before.len().min(self.text.len()));
                let body = format!(
                    "{}{}\x1b[K\x1b[{ROWS};1H{}\x1b[?25h\x1b[?2026l",
                    self.cup(first),
                    &self.text[first..],
                    self.cup(self.caret)
                );
                if self.shape == Shape::ClaudeTorn {
                    self.term.process(b"\x1b[?2026h\x1b[?25l");
                    self.frame();
                    self.now += Duration::from_millis(INK_SPLIT_MS);
                    self.term.process(body.as_bytes());
                } else {
                    self.term
                        .process(format!("\x1b[?2026h\x1b[?25l{body}").as_bytes());
                }
            }
            Shape::Visible => {
                // readline: the edit column on, reprinted, then CUB back to
                // the caret — at a visible caret, one read.
                let first = before
                    .chars()
                    .zip(self.text.chars())
                    .position(|(a, b)| a != b)
                    .unwrap_or_else(|| before.len().min(self.text.len()));
                let mut s = self.text[first..].to_string();
                let back = self.text.len() - self.caret;
                if back > 0 {
                    s.push_str(&format!("\x1b[{back}D"));
                }
                self.term.process(s.as_bytes());
            }
        }
        self.frame();
    }

    /// One typed key `KEY_MS` after the last event, stamped as the host
    /// stamps it.
    fn key(&mut self, ch: char) {
        self.schedule(KEY_MS);
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        let before = self.text.clone();
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.repaint(&before);
    }

    /// One Left arrow `ms` after the last event, stamped as the host stamps
    /// a navigation key (`clear_typed_keeping_hand` then `note_motion`).
    fn left(&mut self, ms: u64) {
        self.schedule(ms);
        self.glow.clear_typed_keeping_hand(self.now);
        self.glow.note_motion(self.now);
        let before = self.text.clone();
        self.caret -= 1;
        if self.shape == Shape::Visible {
            self.term.process(b"\x1b[D");
            self.frame();
        } else {
            self.repaint(&before);
        }
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    /// Runs of consecutive frames with a hole: `(first, last, widest)`.
    fn hole_runs(&self) -> Vec<(usize, usize, Vec<u16>)> {
        let mut runs: Vec<(usize, usize, Vec<u16>)> = Vec::new();
        for (i, f) in self.census.iter().enumerate() {
            if f.holes.is_empty() {
                continue;
            }
            match runs.last_mut() {
                Some(r) if r.1 + 1 == i => {
                    r.1 = i;
                    if f.holes.len() > r.2.len() {
                        r.2 = f.holes.clone();
                    }
                }
                _ => runs.push((i, i, f.holes.clone())),
            }
        }
        runs
    }

    fn dump(&self, from: usize, to: usize) -> String {
        let mut s = String::new();
        for (i, f) in self.census.iter().enumerate().take(to + 1).skip(from) {
            s.push_str(&format!(
                "f={i:04} t={:>6}ms [{}] holes={:?} cells={:?}\n  txt |{}|\n  lit |{}|\n",
                f.ms, f.label, f.holes, f.cells, f.text, f.lit
            ));
        }
        s
    }

    /// No hole stands longer than [`HOLE_FRAMES_MAX`] frames; the frames
    /// around the longest when one does.
    fn assert_no_standing_hole(&self, what: &str) {
        let runs = self.hole_runs();
        if let Some((a, b, cols)) = runs.iter().max_by_key(|(a, b, _)| b - a) {
            assert!(
                b - a < HOLE_FRAMES_MAX,
                "{what}: a hole stood {} frames under {cols:?}\n{}",
                b - a + 1,
                self.dump(a.saturating_sub(2), (b + 2).min(a + 12))
            );
        }
    }
}

/// The line the hand types first.
const LINE: &str = "hello there this is a rainbow line";
/// What it types inside the line once it has walked back into it.
const MID: &str = "abcdef";
/// A settled frame: this long after a mid key's echo, inside the key gap.
const SETTLE_MS: u64 = 70;

const SHAPES: [Shape; 6] = [
    Shape::InkSplit,
    Shape::InkWhole,
    Shape::InkHideFirst,
    Shape::ClaudeWhole,
    Shape::ClaudeTorn,
    Shape::Visible,
];

/// The take: [`LINE`] at 12 cps, `back` ← at `left_ms`, a 300 ms rest,
/// then [`MID`] typed inside the line. Returns the host and the census
/// index of every mid key's settled frame.
fn take(shape: Shape, back: usize, left_ms: u64) -> (Host, Vec<usize>) {
    let mut h = Host::new(shape);
    h.label = "type";
    h.type_str(LINE);
    h.label = "left";
    for _ in 0..back {
        h.left(left_ms);
    }
    h.label = "mid";
    h.schedule(300);
    let mut settled = Vec::new();
    for ch in MID.chars() {
        h.key(ch);
        h.idle(SETTLE_MS);
        settled.push(h.census.len() - 1);
    }
    h.label = "idle";
    h.idle(300);
    (h, settled)
}

/// Every text column of the composer row dark on census frame `i`.
fn dark_text_cols(h: &Host, i: usize) -> Vec<usize> {
    let f = &h.census[i];
    let end = f.text.chars().count();
    (h.col0..end)
        .filter(|&c| f.lit.chars().nth(c) != Some('#'))
        .collect()
}

/// On every shape, at three depths and two arrow paces: every settled frame
/// after a key typed inside the line has the band covering the whole text
/// (prefix, keys, and the pushed tail) with no hole.
#[test]
fn typing_inside_a_lit_line_keeps_the_tail_it_pushes_lit() {
    // The 36 takes replay on their own hosts and clocks on the binary's threads (`par_map`); the
    // first failing take in this order raises its panic.
    let takes: Vec<(Shape, usize, u64)> = SHAPES
        .into_iter()
        .flat_map(|shape| {
            [3usize, 8, 15]
                .into_iter()
                .flat_map(move |back| [60u64, 150].map(|left_ms| (shape, back, left_ms)))
        })
        .collect();
    crate::common::par_map(&takes, |&(shape, back, left_ms)| {
        let (h, settled) = take(shape, back, left_ms);
        let what = format!("{shape:?} back={back} left_ms={left_ms}");
        assert_eq!(
            h.text,
            {
                let at = LINE.len() - back;
                format!("{}{MID}{}", &LINE[..at], &LINE[at..])
            },
            "{what}: the composer holds the edited line"
        );
        for &i in &settled {
            let dark = dark_text_cols(&h, i);
            assert!(
                dark.is_empty(),
                "{what}: text columns {dark:?} dark after a key typed inside the line\n{}",
                h.dump(i.saturating_sub(6), i)
            );
        }
        h.assert_no_standing_hole(&what);
        let tally = h.glow.admission_tally();
        assert_eq!(tally.declined, 0, "{what}: nothing declined");
    });
}

/// Negative control: the same lit line, then no key; the program rewrites the
/// tail with other text (a completion, a reflow). No tail block stands
/// anywhere on the row, so nothing is carried and the light under the
/// replaced letters melts.
#[test]
fn a_tail_the_program_rewrote_still_leaves() {
    for shape in [Shape::Visible, Shape::InkWhole, Shape::ClaudeWhole] {
        let mut h = Host::new(shape);
        h.type_str(LINE);
        h.schedule(300);
        let before = h.text.clone();
        let at = LINE.find("rainbow").expect("the word is in the line");
        h.text.replace_range(at.., "XQZJKVWPUY");
        if shape == Shape::Visible {
            // A program's own reprint at a visible caret.
            let s = format!("{}{}\x1b[K", h.cup(at), &h.text[at..]);
            h.term.process(s.as_bytes());
            h.frame();
        } else {
            h.repaint(&before);
        }
        h.idle(400);
        let last = h.census.len() - 1;
        let f = &h.census[last];
        // By cell, not by the lit map: the plan feathers the band's edge one
        // column past its last cell.
        let kept: Vec<u16> = f
            .cells
            .iter()
            .copied()
            .filter(|&c| usize::from(c) >= h.col0 + at)
            .collect();
        assert!(
            kept.is_empty(),
            "{shape:?}: the rewritten tail kept cells at {kept:?}\n{}",
            h.dump(last, last)
        );
        assert!(
            (h.col0..h.col0 + at).all(|c| f.lit.chars().nth(c) == Some('#')),
            "{shape:?}: the prefix no one touched stays lit\n{}",
            h.dump(last, last)
        );
    }
}
