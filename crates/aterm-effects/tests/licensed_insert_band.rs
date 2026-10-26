// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end [`CursorGlow`] coverage tests for licensed Tab and paste inserts across whole and torn repaint frames.
//! Verifies that licensed inserts light without standing holes while unlicensed or out-of-budget output stays dark.

use crate::common::{
    LIT_COV, caret_sample, class_of, coverage, geom, holes_of, note_blink, observe_caret_row,
    observe_ribbon_rows, rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, GlowConfig, InsertWidth};
use std::time::{Duration, Instant};

const CW: usize = 8;
const CH: usize = 16;
const ROWS: usize = 60;
const COLS: usize = 140;
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
const INK_SPLIT_MS: u64 = 30;
const HOLE_FRAMES_MAX: usize = 3;

const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN!!!!";
const TYPED_TO: &str = "WORK IN";
const INSERT: &str = "TO ";
const SUFFIX: &str = "MAIN!!!!";

/// How the composer's repaint reaches the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Ink-style redraw split across two presents (`?25l` + line, then caret CUP + `?25h`).
    InkSplit,
    /// Ink-style redraw delivered in one read.
    InkWhole,
    /// Hide (`?25l`) presented alone, followed 30 ms later by the line, caret CUP, and `?25h`.
    InkHideFirst,
    /// Claude Code diff frame (`?2026h ?25l … ?25h ?2026l`) delivered in one read.
    ClaudeWhole,
    /// Claude Code diff frame torn after `?2026h ?25l`.
    ClaudeTorn,
    /// Shell completion printed directly at a visible caret.
    Visible,
}

/// Licence stamped by the host for the insert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lic {
    /// Unlicensed program output.
    None,
    /// Bare Tab key (`InsertWidth::Unknown`).
    Tab,
    /// Paste priced at `n` cells (`InsertWidth::Cells(n)`).
    Paste(u16),
}

/// One frame's census of the composer row.
#[derive(Clone, Debug)]
struct Census {
    ms: u64,
    label: &'static str,
    text: String,
    lit: String,
    holes: Vec<u16>,
    cells: Vec<u16>,
}

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
        let mut glow = crate::common::quads_glow();
        glow.note_pane_columns(0, COLS);
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

    /// Observe caret/ribbon rows and print anchor, tick [`CursorGlow`], and record the census.
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

    /// Advance frames to `ms` after `last_event`.
    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = self.now.max(t);
        self.last_event = self.now;
    }

    fn cup(&self, col: usize) -> String {
        format!("\x1b[{};{}H", self.row + 1, self.col0 + col + 1)
    }

    fn ink_halves(&self, line: &str) -> (String, String) {
        (
            format!("\x1b[?25l\x1b[{};1H\x1b[2K{line}", self.row + 1),
            format!("{}\x1b[?25h", self.cup(self.caret)),
        )
    }

    /// Emit the repaint from `before` to `self.text` in `self.shape`.
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
                let first = before
                    .chars()
                    .zip(self.text.chars())
                    .position(|(a, b)| a != b)
                    .unwrap_or_else(|| before.len().min(self.text.len()));
                let body = format!(
                    "{}{}\x1b[{ROWS};1H{}\x1b[?25h\x1b[?2026l",
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
                let tail = self.text[before.len()..].to_string();
                self.term.process(tail.as_bytes());
            }
        }
        self.frame();
    }

    fn key(&mut self, ch: char) {
        self.schedule(KEY_MS);
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        let before = self.text.clone();
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.repaint(&before);
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    /// Stamp `lic` at `gap_ms` after the previous event and repaint `s` after `echo_ms`.
    fn insert(&mut self, s: &str, lic: Lic, gap_ms: u64, echo_ms: u64) {
        self.schedule(gap_ms);
        let press = self.now;
        match lic {
            Lic::None => {}
            Lic::Tab => {
                self.glow.note_user_gesture(press);
                self.glow.note_insert_armed(press, InsertWidth::Unknown);
            }
            Lic::Paste(_) => {
                self.glow.note_user_gesture(press);
                self.glow.revoke_input_hints_at(press);
            }
        }
        self.schedule(echo_ms);
        if let Lic::Paste(n) = lic {
            self.glow
                .note_insert_delivered_from(press, self.now, InsertWidth::Cells(n));
        }
        let before = self.text.clone();
        self.text.insert_str(self.caret, s);
        self.caret += s.chars().count();
        self.repaint(&before);
    }

    /// Type `ch` with a torn Ink redraw showing `shown` on the first present.
    fn torn_key(&mut self, ch: char, shown: &str) {
        self.schedule(KEY_MS);
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.torn_redraw(shown);
    }

    fn torn_repaint(&mut self, shown: &str, ms: u64) {
        self.schedule(ms);
        self.torn_redraw(shown);
    }

    fn torn_redraw(&mut self, shown: &str) {
        let (head, _) = self.ink_halves(shown);
        self.term.process(head.as_bytes());
        self.frame();
        self.now += Duration::from_millis(INK_SPLIT_MS);
        let (head, tail) = self.ink_halves(&self.text);
        self.term
            .process(format!("{}{tail}", &head[6..]).as_bytes());
        self.frame();
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
                "f={i:04} t={:>6}ms [{}] holes={:?}\n  txt |{}|\n  lit |{}|\n",
                f.ms, f.label, f.holes, f.text, f.lit
            ));
        }
        s
    }

    /// Assert that no hole stands for more than [`HOLE_FRAMES_MAX`] frames.
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

    fn insert_cols(&self) -> std::ops::Range<u16> {
        let first = self.col0 + typed_prefix().len();
        (first as u16)..((first + INSERT.len()) as u16)
    }
}

fn typed_prefix() -> &'static str {
    let i = LINE.find(TYPED_TO).expect("the prefix is in the line") + TYPED_TO.len();
    &LINE[..i]
}

fn completed() -> String {
    format!("{}{INSERT}", typed_prefix())
}

fn take(shape: Shape, lic: Lic, gap_ms: u64, echo_ms: u64) -> Host {
    let mut h = Host::new(shape);
    h.label = "type";
    h.type_str(typed_prefix());
    h.label = "insert";
    h.insert(INSERT, lic, gap_ms, echo_ms);
    h.label = "suffix";
    h.type_str(SUFFIX);
    h.label = "idle";
    h.idle(600);
    assert_eq!(h.text, LINE, "the composer ends with the owner's line");
    h
}

/// Assert that a licensed take records one lit insert with zero declines and no standing hole.
fn assert_laid_whole(h: &Host, what: &str) {
    let at = h
        .census
        .iter()
        .position(|f| f.label == "insert")
        .unwrap_or(0);
    let around = h.dump(at.saturating_sub(1), at + 6);
    let tally = h.glow.admission_tally();
    assert_eq!(
        h.glow.insert_tally().lit,
        1,
        "{what}: the insert's echo is laid as the insert, once (last decline {:?})\n{around}",
        tally.last_decline_reason
    );
    assert_eq!(
        tally.declined, 0,
        "{what}: nothing is declined (last {:?})\n{around}",
        tally.last_decline_reason
    );
    h.assert_no_standing_hole(what);
}

fn torn_timings() -> Vec<(Shape, u64, u64)> {
    [Shape::InkHideFirst, Shape::ClaudeTorn]
        .into_iter()
        .flat_map(|shape| [(120, 0), (120, 40), (700, 200)].map(|(gap, echo)| (shape, gap, echo)))
        .collect()
}

/// Verify that a licensed Tab or paste whose frame is torn before its glyphs is laid as an insert.
#[test]
fn a_licensed_insert_whose_frame_is_torn_before_its_glyphs_is_laid_as_the_insert() {
    let takes: Vec<(Lic, Shape, u64, u64)> = [Lic::Tab, Lic::Paste(3)]
        .into_iter()
        .flat_map(|lic| {
            torn_timings()
                .into_iter()
                .map(move |(shape, gap, echo)| (lic, shape, gap, echo))
        })
        .collect();
    crate::common::par_map(&takes, |&(lic, shape, gap, echo)| {
        let h = take(shape, lic, gap, echo);
        assert_laid_whole(&h, &format!("{shape:?} {lic:?} gap={gap} echo={echo}"));
    });
}

/// Verify that a licensed insert is laid whole across every non-torn repaint shape.
#[test]
fn a_licensed_insert_is_laid_whole_in_every_frame_shape_that_reaches_the_glass_whole() {
    let takes: Vec<(Shape, Lic)> = [
        Shape::InkSplit,
        Shape::InkWhole,
        Shape::ClaudeWhole,
        Shape::Visible,
    ]
    .into_iter()
    .flat_map(|shape| [Lic::Tab, Lic::Paste(3)].map(|lic| (shape, lic)))
    .collect();
    crate::common::par_map(&takes, |&(shape, lic)| {
        let h = take(shape, lic, 120, 0);
        assert_laid_whole(&h, &format!("{shape:?} {lic:?}"));
    });
}

/// Verify that unlicensed program output at the caret lays no light in any frame shape.
#[test]
fn program_output_at_the_caret_lays_no_light_however_its_frame_reaches_the_glass() {
    let shapes = [
        Shape::InkSplit,
        Shape::InkWhole,
        Shape::InkHideFirst,
        Shape::ClaudeWhole,
        Shape::ClaudeTorn,
        Shape::Visible,
    ];
    crate::common::par_map(&shapes, |&shape| {
        let h = take(shape, Lic::None, 120, 0);
        let cols = h.insert_cols();
        let lit_under = h
            .census
            .iter()
            .enumerate()
            .find(|(_, f)| f.cells.iter().any(|c| cols.contains(c)));
        assert!(
            lit_under.is_none(),
            "{shape:?}: the program's `TO ` at {cols:?} carries a cell\n{}",
            lit_under.map_or(String::new(), |(i, _)| h.dump(i.saturating_sub(1), i + 2))
        );
        assert_eq!(h.glow.insert_tally().lit, 0, "{shape:?}: no insert laid");
    });
}

/// Verify that a torn reappearance exceeding the paste width or past the Tab freshness window stays dark.
#[test]
fn a_torn_reappearance_the_licence_cannot_buy_lays_nothing() {
    let cases = [None, Some(Shape::InkHideFirst), Some(Shape::ClaudeTorn)];
    crate::common::par_map(&cases, |&case| {
        let Some(shape) = case else {
            let mut h = Host::new(Shape::InkHideFirst);
            h.type_str(typed_prefix());
            h.insert("TO MAIN! ", Lic::Paste(3), 120, 0);
            let first = h.col0 + typed_prefix().len();
            let wide = (first as u16)..((first + 9) as u16);
            assert_eq!(
                h.glow.insert_tally().lit,
                0,
                "a 9-cell hop is not a 3-cell paste"
            );
            assert!(
                !h.census
                    .iter()
                    .any(|f| f.cells.iter().any(|c| wide.contains(c))),
                "the hop the paste could not buy lays nothing"
            );
            return;
        };
        let mut h = Host::new(shape);
        h.type_str(typed_prefix());
        h.insert(INSERT, Lic::Tab, 120, 1200);
        let cols = h.insert_cols();
        assert_eq!(
            h.glow.insert_tally().lit,
            0,
            "{shape:?}: the Tab's window closed"
        );
        assert!(
            !h.census
                .iter()
                .any(|f| f.cells.iter().any(|c| cols.contains(c))),
            "{shape:?}: a lapsed Tab's echo lays nothing"
        );
    });
}

/// Verify that a one-frame Ink tear of the completed word heals while typing continues.
#[test]
fn a_one_frame_tear_of_the_completed_word_heals_while_the_hand_types() {
    fn blank_to(text: &str) -> String {
        let i = typed_prefix().len();
        let mut s = text.to_owned();
        s.replace_range(i..i + INSERT.len(), "   ");
        s
    }
    fn cut_at_to(text: &str) -> String {
        text[..typed_prefix().len()].to_owned()
    }
    fn other_glyph_to(text: &str) -> String {
        let i = typed_prefix().len();
        let mut s = text.to_owned();
        s.replace_range(i..i + 2, "XX");
        s
    }
    fn cut_last_two(text: &str) -> String {
        text[..text.len() - 2].to_owned()
    }
    type Tear = fn(&str) -> String;
    let tears: [(&str, Tear, bool); 7] = [
        ("b1 TO blanked, keyed", blank_to, true),
        ("b2 TO blanked, keyless", blank_to, false),
        ("b3 cut at TO, keyed", cut_at_to, true),
        ("b4 cut at TO, keyless", cut_at_to, false),
        ("b5 TO -> XX, keyed", other_glyph_to, true),
        ("b6 TO -> XX, keyless", other_glyph_to, false),
        ("b8 cut one short, keyed", cut_last_two, true),
    ];
    let cases: Vec<Option<(&str, Tear, bool)>> =
        tears.into_iter().map(Some).chain([None]).collect();
    crate::common::par_map(&cases, |&case| {
        let Some((what, tear, keyed)) = case else {
            let mut h = Host::new(Shape::InkSplit);
            h.type_str(&completed());
            h.label = "torn";
            let mut text = h.text.clone();
            text.insert(h.caret, 'M');
            h.torn_key('M', &cut_at_to(&text));
            h.type_str("AIN!!!!");
            h.idle(600);
            assert_eq!(h.text, LINE);
            h.assert_no_standing_hole("b7 cut at TO on the M key");
            return;
        };
        let mut h = Host::new(Shape::InkSplit);
        h.type_str(&completed());
        h.type_str("MAI");
        h.label = "torn";
        if keyed {
            let mut text = h.text.clone();
            text.insert(h.caret, 'N');
            h.torn_key('N', &tear(&text));
        } else {
            let shown = tear(&h.text.clone());
            h.torn_repaint(&shown, 40);
            h.label = "type";
            h.key('N');
        }
        h.type_str("!!!!");
        h.idle(600);
        assert_eq!(h.text, LINE);
        h.assert_no_standing_hole(what);
    });
}
