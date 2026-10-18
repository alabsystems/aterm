// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A licensed insert is laid, however its frame is torn.
//!
//! Cells the program writes at the caret after a Tab or a paste (a
//! completion, a pasted word) must be laid, so the hand's next key does not
//! start a second band with a dark gap between.
//!
//! The take: a line typed to `…BEST WORK IN` at 12 cps, the program inserts
//! `TO ` at the caret (a Tab's completion, a three-cell paste, or no key at
//! all), and the hand types `MAIN!!!!`. The insert's licence is stamped as the
//! host stamps it: a Tab is `note_user_gesture` and
//! `note_insert_armed(Unknown)` at the press (`app_input.rs`); a ⌘V is the
//! gesture stamped and revoked at the enqueue and
//! `note_insert_delivered_from(Cells(3))` at the delivery edge (`input_paste`,
//! `apply_delivery`). The repaint arrives in one of six shapes ([`Shape`]):
//! the Ink-shaped split whole-line redraw, the same bytes in one read, the same
//! bytes with the hide alone in the first read, Claude Code's own diff frame
//! (`?2026h ?25l … ?25h ?2026l`) whole or torn after its hide, and a shell's
//! print at a visible caret.
//!
//! The hard case is a frame torn before its glyphs (the hide alone in the
//! first read): the next present shows the caret the insert's width on, with
//! the print arriving in the same present. `CursorGlow::hidden_bridge_source`
//! bridges a hidden→visible reappearance that a fresh insert licence lays,
//! with the insert's reach and whatever the hide's age. Its predicate is the
//! one `spawn` lays the insert by (`visible_insert_echo`), so it admits
//! nothing the insert arm refuses: a reappearance wider than the paste, a
//! Tab's echo past its one-second window, and unlicensed output stay dark.
//!
//! Unlicensed program output (T1, v3's X1) keeps its cells dark; the hand's
//! next key past them still starts a second band, with the gap beside it kept
//! while the hand types. That behaviour is not pinned either way here.
//!
//! The census is the plan's coverage on the composer row every frame: `#` at
//! ≥ 20 coverage, `.` dark. A hole is a dark column strictly between two lit
//! ones. A licensed take allows none lasting longer than [`HOLE_FRAMES_MAX`]
//! frames, from the first key to the end of the take.

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
/// 12 cps, the owner's pace.
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
/// The Ink-shaped composer's pause between a redraw's two reads.
const INK_SPLIT_MS: u64 = 30;
/// A hole standing longer than this many frames is the defect.
const HOLE_FRAMES_MAX: usize = 3;

/// The owner's line.
const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN!!!!";
/// Where the hand's typing stops before the insert.
const TYPED_TO: &str = "WORK IN";
/// What the program inserts at the caret.
const INSERT: &str = "TO ";
/// What the hand types after it.
const SUFFIX: &str = "MAIN!!!!";

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
    /// A shell completing at a visible caret: the glyphs printed where the
    /// caret stands, no hide.
    Visible,
}

/// The licence the host stamps for the insert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lic {
    /// Program output: no key behind it.
    None,
    /// A bare Tab.
    Tab,
    /// A ⌘V priced at `n` cells.
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

    /// Exactly the render path: the frame hold's caret row and every ribbon
    /// row whether or not the caret is visible, the print anchor fed just
    /// before the tick (`app_render.rs`), the tick, then the census.
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
                // The diff: from the first changed text column on.
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

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    /// The insert: its licence stamped `gap_ms` after the last key, the
    /// program's repaint of it `echo_ms` after that.
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
            // The delivery edge, applied before the frame that shows the
            // echo.
            self.glow
                .note_insert_delivered_from(press, self.now, InsertWidth::Cells(n));
        }
        let before = self.text.clone();
        self.text.insert_str(self.caret, s);
        self.caret += s.chars().count();
        self.repaint(&before);
    }

    /// A key whose Ink redraw is torn: the first present shows `shown` with
    /// the caret hidden, the second, 30 ms later, the whole text and caret.
    fn torn_key(&mut self, ch: char, shown: &str) {
        self.schedule(KEY_MS);
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.torn_redraw(shown);
    }

    /// A program repaint `ms` after the last event, torn the same way.
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

    /// The grid columns the insert took.
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

/// The take: the hand types to `…WORK IN`, the insert lands, the hand types
/// `MAIN!!!!`, then 600 ms idle.
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

/// A licensed take is laid as the insert, once, and leaves no hole; the
/// frames around the insert when it does not.
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

/// Every torn shape at each `(gap, echo)` timing, in that order.
fn torn_timings() -> Vec<(Shape, u64, u64)> {
    [Shape::InkHideFirst, Shape::ClaudeTorn]
        .into_iter()
        .flat_map(|shape| [(120, 0), (120, 40), (700, 200)].map(|(gap, echo)| (shape, gap, echo)))
        .collect()
}

// Every law below replays independent takes, each its own host on its own
// clock, so each runs them on the binary's threads (`crate::common::par_map`)
// and the first failing take in the law's order raises its own panic (0.8 to
// 4.0 s a law alone on one thread).

/// A tab completion or a paste whose frame is torn before its glyphs. Before
/// the fix the Tab was declined `hidden-relocation`, `O` and ` ` of `INTO `
/// dark for 73 frames in every take; the paste was refused silently (the
/// enqueue revoked its gesture), the same 73-frame hole.
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

/// Controls: every shape whose print or caret move arrives whole is laid, by
/// the anchored lane (the split redraw's print under the hidden caret) or the
/// insert arm (a visible hop).
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

/// T1 and v3's X1: program output at the caret lays no cell, caret visible or
/// hidden, torn or whole; the bridge lane opens only for a licence. (The gap
/// beside these cells when the hand types on is not pinned.)
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

/// The bridge admits only what the licence buys: a torn reappearance wider
/// than the paste's priced width, or a Tab's torn echo after its one-second
/// window (`INSERT_GESTURE_HINT_FRESH`), is refused and stays dark.
#[test]
fn a_torn_reappearance_the_licence_cannot_buy_lays_nothing() {
    // `None`: the paste; `Some(shape)`: the lapsed Tab in that shape.
    let cases = [None, Some(Shape::InkHideFirst), Some(Shape::ClaudeTorn)];
    crate::common::par_map(&cases, |&case| {
        let Some(shape) = case else {
            // A three-cell paste, then the program re-lays the line nine cells
            // on.
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
        // A Tab whose completion lands 1.2 s after the press.
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

/// Torn-frame guard: with the hand typing `MAIN!!!!` after `INTO `, one Ink
/// repaint shows the line damaged for a frame and the next restores it. Every
/// shape heals: three interior cells blanked or replaced, the suffix from
/// `TO ` cut off (a 1024-byte tear), on the first key after `INTO `, or one
/// cell short of the caret, under a key or between keys.
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
    // The seven tears, then (`None`) b7.
    let cases: Vec<Option<(&str, Tear, bool)>> =
        tears.into_iter().map(Some).chain([None]).collect();
    crate::common::par_map(&cases, |&case| {
        let Some((what, tear, keyed)) = case else {
            // b7: the tear on the `M` key, the torn present showing `…WORK IN`.
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
