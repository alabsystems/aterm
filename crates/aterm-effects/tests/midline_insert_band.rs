// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **TYPING INSIDE A LIT LINE KEEPS THE TAIL IT PUSHES LIT** (2026-09-24).
//! The owner, on Linux, in Claude Code's composer: *"when I did a backward
//! movement, the rainbow cursor trail fractured with black spaces when I
//! started typing in the middle of a line"*.
//!
//! The take: a line typed at 12 cps, the caret walked back into it with ←
//! (the host stamps a navigation key: `clear_typed`, then `note_motion`),
//! a rest, then six keys typed INSIDE the line. Every key is an insert: the
//! program rewrites the rest of the line one column on. The content witness
//! read each of those columns as REPLACED text and melted the whole tail in
//! 120 ms — the band went black from the caret to the end of the line on
//! the first key, and each later edit cut another dark stretch out of the
//! rainbow. Measured before the fix on every shape below: the lit map ended
//! at the caret on every settled frame after the first key.
//!
//! **The fix** is the shift pass (`Witness::shift_runs`,
//! `Ribbon::shift_run`, run by `Engine::follow_rows` before the tick): the
//! tail's glyphs are found where the insert pushed them and their light
//! moves with them, clocks intact; and a key a hidden-caret composer
//! reports at the PRINT's end (the line's end) is laid at the insert it made
//! (`Ribbon::insert_landing`).
//!
//! The repaint reaches the glass in six shapes ([`Shape`]): `inkish.py`'s
//! split whole-line redraw, the same bytes in one read, the hide alone in
//! the first read, Claude Code's own diff frame whole or torn after its
//! hide, and readline's visible reprint-and-`CUB`. The census is the PLAN's
//! coverage on the composer row: `#` at ≥ 20 coverage, `.` dark.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

const CW: usize = 8;
const CH: usize = 16;
const ROWS: usize = 60;
const COLS: usize = 140;
/// 12 cps, the owner's pace.
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
/// `inkish.py`'s pause between a redraw's two reads.
const INK_SPLIT_MS: u64 = 30;
/// A column is LIT at this planned coverage (the glass census's 20/255).
const LIT_COV: u8 = 20;
/// A hole standing longer than this many frames is the defect.
const HOLE_FRAMES_MAX: usize = 3;

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

/// The owner's config: rainbow kitty, tall body, intensity 0.70, dark.
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
        audible: true,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
    }
}

/// The host's `typed_class_for` and `glyph_shifted && !spacebar`.
fn class_of(ch: char) -> (bool, TypedClass) {
    match ch {
        ' ' => (false, TypedClass::Space),
        '!' => (true, TypedClass::Bang),
        c if c.is_uppercase() => (true, TypedClass::Capital),
        _ => (false, TypedClass::Glyph),
    }
}

/// How the composer's repaint reaches the glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// `inkish.py`: hide, home, clear, the whole line | present | 30 ms |
    /// the caret placed and shown | present.
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
    /// A shell completing at a visible caret: the glyphs printed where the
    /// caret stands, no hide.
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
            cfg: cfg(),
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

    /// EXACTLY the render path: the frame hold's caret row and every ribbon row
    /// whether or not the caret is visible, the print anchor fed immediately
    /// before the tick (`app_render.rs`), the tick, then the census.
    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink {
            self.blink = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
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
        self.glow.observe_print_anchor(self.term.print_anchor());
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
        self.record();
    }

    fn record(&mut self) {
        let row = self.row;
        let mut cov = [0u8; COLS];
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        for sg in rib.plan_segments() {
            let r = ((sg.spine / CH as f32) - 0.5).floor();
            let c = (sg.x / CW as f32).floor();
            if r >= 0.0 && c >= 0.0 && r as u16 == row && (c as usize) < COLS {
                let i = c as usize;
                cov[i] = cov[i].max(sg.cov);
            }
        }
        let lit: Vec<usize> = (0..COLS).filter(|&i| cov[i] >= LIT_COV).collect();
        let holes = if lit.len() >= 2 {
            (lit[0]..=lit[lit.len() - 1])
                .filter(|&i| cov[i] < LIT_COV)
                .map(|i| i as u16)
                .collect()
        } else {
            Vec::new()
        };
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
    /// a navigation key (`clear_typed` then `note_motion`).
    fn left(&mut self, ms: u64) {
        self.schedule(ms);
        self.glow.clear_typed(self.now);
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

/// **THE LAW**, on every shape, at three depths into the line and two arrow
/// paces: on every settled frame after a key typed inside the line, the
/// band covers the WHOLE text — the prefix, the keys, and the tail they
/// pushed — and no hole stands in it.
#[test]
fn typing_inside_a_lit_line_keeps_the_tail_it_pushes_lit() {
    for shape in SHAPES {
        for back in [3usize, 8, 15] {
            for left_ms in [60u64, 150] {
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
            }
        }
    }
}

/// **TEXT A PROGRAM REPLACED DID NOT MOVE** — the negative control. The
/// same lit line, and then no key at all: the program rewrites the tail
/// with other text (a completion, a reflow). No block of the tail stands
/// anywhere along the row, so nothing is carried, and the light under the
/// replaced letters leaves as it always did (the witness's melt).
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
        // By CELL, not by the lit map: the plan feathers the band's edge
        // one column past its last cell, which is the band ending, not
        // light carried onto the rewritten text.
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
