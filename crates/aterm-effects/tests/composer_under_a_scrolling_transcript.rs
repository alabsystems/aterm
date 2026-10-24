// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration tests for composer stability under a scrolling transcript.

use std::time::Duration;

use aterm_core::terminal::{ContentScrollDelta, ContentScrollState};
use aterm_effects::cursor_glow::{AdmissionTally, CURSOR_WITNESS_ROWS, InsertWidth};
use aterm_effects::host::{hidden_caret_withheld, motion_caret, motion_print_anchor};
use aterm_effects::rainbow_kitty::TypedClass;

use crate::trail_host::{Core, Opts, Theme};

const ROWS: usize = 30;
const COLS: usize = 120;
const LAB_COLS: usize = 144;
const TEXT_ROW: u16 = (ROWS - 4) as u16;
const COL0: u16 = 2;
const URL: &str = "https://claude.ai/artifact/XB78QX8U9sp935PNjq8FpB";
const KEY_MS: u64 = 115;
const FRAME_MS: u64 = 16;
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

struct Claude {
    h: Core,
    /// The screen's columns.
    cols: usize,
    /// The composer's caret row: [`TEXT_ROW`] under a full transcript, higher
    /// on a screen the transcript has not filled yet, until tool steps push
    /// the box down to the bottom.
    row: u16,
    /// The composer's text so far.
    text: String,
    /// Tool results printed so far.
    steps: usize,
    /// Band cells off the composer on each tool result's own frame.
    on_scroll_frames: Vec<(u16, u16)>,
    /// The composer's first line, in glyphs, when the text wraps: past it the
    /// box grows a row and the rest is on a second line under it.
    wrap: Option<usize>,
    /// Claude Code's word wrap at a space (the lab's cast, 2.1.305): the Space
    /// typed past [`Self::wrap`] stays a blank at the first line's end, the
    /// second line starts empty, and its echo rewrites the box from the
    /// composer down, the rows under it one lower.
    space_wrap: bool,
}

impl Claude {
    fn new() -> Self {
        let c = Self::with_transcript(ROWS + 10, COLS);
        assert_eq!(c.row, TEXT_ROW, "fixture: the box is on the bottom rows");
        c
    }

    fn with_transcript(lines: usize, cols: usize) -> Self {
        let h = Core::new(
            ROWS,
            cols,
            8,
            16,
            Theme::Default,
            Opts {
                scroll_seam: true,
                alt_rebaseline: true,
                skip_caret: true,
                honour_sync: true,
                pane_columns: true,
            },
        );
        let mut s = String::from("\x1b[?25l");
        for i in 0..lines {
            s.push_str(&format!("  transcript line {i} of an earlier turn\r\n"));
        }
        let mut c = Self {
            h,
            cols,
            row: 0,
            text: String::new(),
            steps: 0,
            on_scroll_frames: Vec::new(),
            wrap: None,
            space_wrap: false,
        };
        s.push_str(&c.region());
        s.push_str(&format!("\r\x1b[{}C\x1b[3A", c.caret_col()));
        c.h.term.process(s.as_bytes());
        c.row = c.h.term.cursor().row;
        c.frame();
        c.idle(200);
        c
    }

    fn lines(&self) -> (String, Option<String>) {
        if let Some((first, second)) = self.text.split_once('\n') {
            return (first.to_string(), Some(second.to_string()));
        }
        match self.wrap {
            Some(w) if self.space_wrap && self.text.chars().count() > w => {
                assert_eq!(
                    self.text.chars().nth(w),
                    Some(' '),
                    "a word wrap is at a space"
                );
                (
                    self.text.chars().take(w + 1).collect(),
                    Some(self.text.chars().skip(w + 1).collect()),
                )
            }
            Some(w) if self.text.chars().count() > w => (
                self.text.chars().take(w).collect(),
                Some(self.text.chars().skip(w).collect()),
            ),
            _ => (self.text.clone(), None),
        }
    }

    fn caret_col(&self) -> u16 {
        let (first, second) = self.lines();
        COL0 + second.unwrap_or(first).chars().count() as u16
    }

    fn region_rows(&self) -> usize {
        if self.lines().1.is_some() { 9 } else { 8 }
    }

    fn region(&self) -> String {
        let rule = "\u{2500}".repeat(self.cols);
        let rows = [
            format!(
                "\u{2733} Tomfoolering\u{2026} ({}s \u{b7} \u{2193} 339 tokens)",
                7 + self.steps
            ),
            "  \u{23bf} \u{a0}Tip: Use /statusline to set up a custom status line".to_string(),
            String::new(),
            rule.clone(),
        ];
        let composer = match self.lines() {
            (first, None) => vec![format!("\u{276f}\u{a0}{first}\x1b[7m \x1b[27m")],
            (first, Some(second)) => vec![
                format!("\u{276f}\u{a0}{first}"),
                format!("  {second}\x1b[7m \x1b[27m"),
            ],
        };
        let footer = [
            rule,
            "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle)".to_string(),
            String::new(),
        ];
        rows.iter()
            .chain(&composer)
            .chain(&footer)
            .map(|r| format!("{r}\x1b[K"))
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    fn frame(&mut self) {
        if self.h.term.sync_open_dirty() {
            return;
        }
        self.present();
    }

    fn present(&mut self) {
        let h = &mut self.h;
        let scroll = h.term.content_scroll_state();
        let scrolled = match ContentScrollState::delta_since(h.scroll, scroll) {
            ContentScrollDelta::Baseline | ContentScrollDelta::Unchanged => false,
            ContentScrollDelta::Translate(rows) => {
                h.glow.note_scroll(rows);
                h.glow.drop_row_probe();
                true
            }
            other => panic!("Claude Code's frames scroll the whole screen, not {other:?}"),
        };
        h.scroll = Some(scroll);
        let c = h.term.cursor();
        let visible = h.term.cursor_visible();
        let cur = motion_caret(
            Some((c.row, c.col)),
            visible,
            hidden_caret_withheld(&h.term),
        );
        let epoch = h.term.repaint_blink_epoch();
        if epoch != h.blink_seen {
            h.blink_seen = epoch;
            h.glow.note_repaint_blink(h.now);
        }
        h.glow.note_context(h.term.is_alternate_screen());
        h.term.row_cols_into(usize::from(c.row), &mut h.row_buf);
        if !scrolled {
            h.glow.observe_row(c.row, c.col, &h.row_buf, h.now);
        }
        h.glow.observe_ribbon_row(c.row, &h.row_buf);
        let mut rows = [0u16; CURSOR_WITNESS_ROWS];
        let n = h.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            if r == c.row {
                continue;
            }
            h.term.row_cols_into(usize::from(r), &mut h.row_buf);
            h.glow.observe_ribbon_row(r, &h.row_buf);
        }
        let (anchor, glyph) = motion_print_anchor(&h.term);
        h.glow.observe_print_anchor_glyph(anchor, glyph);
        h.glow.observe_caret_drawn(visible);
        h.glow.tick(cur, h.now, &h.cfg, h.g, &mut h.out);
    }

    fn step(&mut self) {
        self.h.now += Duration::from_millis(FRAME_MS);
        self.frame();
    }

    fn idle(&mut self, ms: u64) {
        for _ in 0..ms / FRAME_MS {
            self.step();
        }
    }

    fn echo(&mut self, s: &str) {
        let c = self.caret_col();
        let grows = self.region_rows();
        self.text.push_str(s);
        if self.region_rows() > grows && self.space_wrap {
            let bytes = format!(
                "{SYNC_BEGIN}\x1b[{c}D\x1b[3B\r\x1b[{c}C\x1b[3A\x1b[K\r\x1b[1B  \x1b[7m \x1b[27m\x1b[K\r\x1b[1B{}\r\r\n  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle)\x1b[K\r\r\n\x1b[2C\x1b[3A{SYNC_END}",
                "\u{2500}".repeat(self.cols)
            );
            self.h.term.process(bytes.as_bytes());
            self.row = self.h.term.cursor().row;
            return;
        }
        if self.region_rows() > grows {
            let bytes = format!(
                "{SYNC_BEGIN}\x1b[{c}D\x1b[3B\r\x1b[{}A{}\r\x1b[{}C\x1b[3A{SYNC_END}",
                grows - 1,
                self.region(),
                self.caret_col()
            );
            self.h.term.process(bytes.as_bytes());
            self.row = self.h.term.cursor().row;
            return;
        }
        let bytes = format!(
            "{SYNC_BEGIN}\x1b[{c}D\x1b[3B\r\x1b[{c}C\x1b[3A{s}\x1b[7m \x1b[27m\r\r\n\r\n\r\n\x1b[{}C\x1b[3A{SYNC_END}",
            self.caret_col()
        );
        self.h.term.process(bytes.as_bytes());
    }

    fn press(&mut self, ch: char) {
        self.idle(KEY_MS - 2);
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        let now = self.h.now;
        self.h.glow.note_typed_expected(now, 1, false, class, ch);
        self.h.now += Duration::from_millis(2);
    }

    fn key(&mut self, ch: char) {
        self.press(ch);
        self.echo(&ch.to_string());
        self.frame();
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    fn paste(&mut self, s: &str) {
        self.deliver(s);
        self.echo(s);
        self.frame();
    }

    fn deliver(&mut self, s: &str) {
        let cells = u16::try_from(s.chars().count()).expect("a short paste");
        self.deliver_as(InsertWidth::Cells(cells));
    }

    fn deliver_as(&mut self, width: InsertWidth) {
        self.idle(KEY_MS - 2);
        let at = self.h.now;
        self.h.glow.note_user_gesture(at);
        self.h.glow.revoke_input_hints_at(at);
        self.h.now += Duration::from_millis(1);
        let delivered = self.h.now;
        self.h.glow.note_insert_delivered_from(at, delivered, width);
        self.h.now += Duration::from_millis(1);
    }

    fn tool_result(&mut self) {
        self.tool_rows(3);
    }

    fn tool_rows(&mut self, rows: usize) {
        self.land_tool_rows(rows);
        self.frame();
        let off = self.stray();
        self.on_scroll_frames.extend(off);
    }

    fn land_tool_rows(&mut self, rows: usize) {
        self.land_tool_rows_unfinished(rows);
        self.h.term.process(SYNC_END.as_bytes());
    }

    fn land_tool_rows_unfinished(&mut self, rows: usize) {
        self.steps += 1;
        let c = self.caret_col();
        let mut s = format!(
            "{SYNC_BEGIN}\x1b[{c}D\x1b[3B\r\x1b[{}A",
            self.region_rows() - 1
        );
        for i in 0..rows {
            let line = match i {
                0 => format!("\u{23fa} Bash(sleep 0.6; echo step {})", self.steps),
                1 => format!("  \u{23bf}  step {}", self.steps),
                _ => String::new(),
            };
            s.push_str(&format!("{line}\x1b[K\r\n"));
        }
        s.push_str(&self.region());
        s.push_str(&format!("\r\x1b[{c}C\x1b[3A"));
        let scrolled = self.h.term.content_scroll_state();
        self.h.term.process(s.as_bytes());
        let rows = u16::try_from(rows).expect("a short step");
        assert_eq!(
            scrolled != self.h.term.content_scroll_state(),
            self.row + rows > TEXT_ROW,
            "the tool result scrolled the screen exactly when the box reached the bottom"
        );
        let row = (self.row + rows).min(TEXT_ROW);
        assert_eq!(
            (self.h.term.cursor().row, self.h.term.cursor().col),
            (row, c),
            "the caret is back on the composer"
        );
        self.row = row;
    }

    fn verdicts_since(&self, judged: AdmissionTally) -> String {
        let now = self.h.glow.admission_tally();
        let since = (now.licensed + now.declined) - (judged.licensed + judged.declined);
        let log: Vec<_> = self.h.glow.admission_log().collect();
        log[log.len() - usize::try_from(since).expect("a short take")..]
            .iter()
            .map(|r| format!("{} {:?}->{:?} {}", r.reason, r.origin, r.target, r.licence))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn dark_text(&self) -> Vec<u16> {
        let live = self.h.live(self.row);
        let mut dark: Vec<u16> = (COL0..self.caret_col())
            .filter(|c| !live.contains(c))
            .collect();
        if let (first, Some(_)) = self.lines() {
            let live = self.h.live(self.row - 1);
            dark.extend((COL0..COL0 + first.chars().count() as u16).filter(|c| !live.contains(c)));
        }
        dark
    }

    fn stray(&self) -> Vec<(u16, u16)> {
        let top = self.row - u16::from(self.lines().1.is_some());
        (0..top)
            .flat_map(|r| self.h.cells(r).into_iter().map(move |(c, _)| (r, c)))
            .collect()
    }
}

fn take(tool_results: bool) -> (Vec<u16>, Vec<(u16, u16)>, u64, String) {
    let mut c = Claude::new();
    c.type_str("check ");
    c.paste(URL);
    if tool_results {
        c.idle(120);
        c.tool_result();
    }
    c.idle(60);
    let after_paste = (c.dark_text(), c.stray());
    c.type_str(" these kit");
    let mut after_keys = Vec::new();
    if tool_results {
        c.idle(40);
        c.tool_result();
        c.idle(40);
        after_keys = c.stray();
    }
    c.type_str("ties are a bit TOO cute.");
    c.idle(60);
    let note = format!(
        "after the paste: dark {:?}, stray {:?}; after the second scroll: stray {after_keys:?}; \
         on the scrolls' own frames: {:?}",
        after_paste.0, after_paste.1, c.on_scroll_frames
    );
    let (mut dark, mut stray) = (c.dark_text(), c.stray());
    dark.extend(after_paste.0);
    stray.extend(after_paste.1);
    stray.extend(after_keys);
    stray.extend(c.on_scroll_frames);
    dark.sort_unstable();
    dark.dedup();
    (dark, stray, c.h.counts().1, note)
}

#[test]
fn the_owners_take_with_no_tool_result_is_lit() {
    let (dark, stray, retired, note) = take(false);
    assert!(dark.is_empty(), "dark composer columns {dark:?} ({note})");
    assert!(
        stray.is_empty(),
        "band cells off the composer {stray:?} ({note})"
    );
    assert_eq!(retired, 0, "nothing melts ({note})");
}

#[test]
fn a_paste_and_the_keys_after_it_keep_their_band_through_a_tool_result_scroll() {
    let (dark, stray, retired, note) = take(true);
    assert!(dark.is_empty(), "dark composer columns {dark:?} ({note})");
    assert!(
        stray.is_empty(),
        "band cells off the composer {stray:?} ({note})"
    );
    assert_eq!(retired, 0, "nothing melts ({note})");
}

type RestSample = (Vec<u16>, Vec<(u16, u16)>);

fn rest_after_paste(tool_result: bool) -> Vec<RestSample> {
    let mut c = Claude::new();
    c.type_str("check ");
    c.paste(URL);
    c.idle(120);
    if tool_result {
        c.tool_result();
    } else {
        c.frame();
    }
    (0..40)
        .map(|_| {
            c.idle(100);
            (c.h.live(TEXT_ROW), c.stray())
        })
        .collect()
}

#[test]
fn a_band_at_rest_lives_the_same_life_through_a_tool_result_scroll() {
    let calm = rest_after_paste(false);
    let scrolled = rest_after_paste(true);
    assert!(
        !calm[0].0.is_empty(),
        "the paste is lit after it: {:?}",
        calm[0]
    );
    for (i, (want, got)) in calm.iter().zip(&scrolled).enumerate() {
        assert_eq!(got, want, "{} ms into the rest", (i + 1) * 100);
    }
}

/// How a tool step lands between a key's press and its echo, as the lab's
/// recording of Claude Code 2.1.304 shows it: the step's result in place of its
/// `Running…` row (a scroll of one), then 16 ms later the next call's line,
/// `Running…` row and blank (a scroll of three).
#[derive(Clone, Copy, Debug)]
enum Step {
    /// Each on its own frame, the echo 10 ms after the second.
    TwoFrames,
    /// Both before the next frame (one scroll of four), the echo 10 ms later.
    OneFrame,
    /// Each on its own frame, the echo on a later frame 25 ms after the second.
    EchoLater,
}

fn step_under_a_key(step: Step) -> (Vec<u16>, Vec<(u16, u16)>, u64, String) {
    let mut c = Claude::new();
    c.type_str("check ");
    c.paste(URL);
    c.idle(60);
    c.type_str(" th");
    let judged = c.h.glow.admission_tally();
    c.press('e');
    c.h.now += Duration::from_millis(11);
    match step {
        Step::TwoFrames | Step::EchoLater => {
            c.tool_rows(1);
            c.idle(FRAME_MS);
            c.tool_rows(3);
        }
        Step::OneFrame => {
            c.land_tool_rows(1);
            c.h.now += Duration::from_millis(FRAME_MS);
            c.land_tool_rows(3);
            c.frame();
        }
    }
    if matches!(step, Step::EchoLater) {
        c.idle(2 * FRAME_MS);
    }
    c.h.now += Duration::from_millis(10);
    c.echo("e");
    c.frame();
    c.idle(48);
    let verdicts = c.verdicts_since(judged);
    c.type_str("se kitties are a bit TOO cute.");
    c.idle(60);
    let mut stray = c.stray();
    stray.extend(c.on_scroll_frames.iter().copied());
    (c.dark_text(), stray, c.h.counts().1, verdicts)
}

#[test]
fn a_key_pressed_as_a_tool_step_lands_keeps_its_press_for_its_echo() {
    for step in [Step::TwoFrames, Step::OneFrame, Step::EchoLater] {
        let (dark, stray, retired, verdicts) = step_under_a_key(step);
        assert!(
            dark.is_empty(),
            "{step:?}: dark composer columns {dark:?} (verdicts: {verdicts})"
        );
        assert!(
            stray.is_empty(),
            "{step:?}: band cells off the composer {stray:?} (verdicts: {verdicts})"
        );
        assert_eq!(retired, 0, "{step:?}: nothing melts (verdicts: {verdicts})");
        assert_eq!(
            verdicts,
            format!("licensed {:?}->{:?} key", (TEXT_ROW, 60), (TEXT_ROW, 61)),
            "{step:?}: the echo is the one move judged from the press on, licensed by its key"
        );
    }
}

fn paste_in_a_step(typed: &str) -> (Vec<u16>, Vec<(u16, u16)>, u64, String) {
    let mut c = Claude::new();
    c.type_str(typed);
    c.deliver(URL);
    let judged = c.h.glow.admission_tally();
    c.echo(URL);
    c.tool_result();
    let verdict = c.verdicts_since(judged);
    c.idle(60);
    c.type_str(" these kitties are a bit TOO cute.");
    c.idle(60);
    let mut stray = c.stray();
    stray.extend(c.on_scroll_frames.iter().copied());
    (c.dark_text(), stray, c.h.counts().1, verdict)
}

#[test]
fn a_paste_echoed_in_a_tool_result_s_frame_is_lit() {
    for typed in ["", "check "] {
        let (dark, stray, retired, verdict) = paste_in_a_step(typed);
        let from = COL0 + u16::try_from(typed.len()).expect("short");
        assert_eq!(
            verdict,
            format!(
                "licensed {:?}->{:?} insert",
                (TEXT_ROW, from),
                (TEXT_ROW, from + 49)
            ),
            "after {typed:?}: the echo is judged in its line's own frame"
        );
        assert!(
            dark.is_empty(),
            "after {typed:?}: dark composer columns {dark:?}"
        );
        assert!(
            stray.is_empty(),
            "after {typed:?}: band cells off the composer {stray:?}"
        );
        assert_eq!(retired, 0, "after {typed:?}: nothing melts");
    }
}

fn one_glyph_under_a_step(rows: usize) -> (Vec<u16>, Vec<(u16, u16)>, u64, String) {
    let mut c = Claude::new();
    c.type_str("t");
    c.idle(300);
    let judged = c.h.glow.admission_tally();
    c.tool_rows(rows);
    c.idle(200);
    let verdicts = c.verdicts_since(judged);
    let mut stray = c.stray();
    stray.extend(c.on_scroll_frames.iter().copied());
    (c.dark_text(), stray, c.h.counts().1, verdicts)
}

#[test]
fn a_one_glyph_line_returns_with_the_box_under_a_tool_step() {
    for rows in [1, 3, 4] {
        let (dark, stray, retired, verdicts) = one_glyph_under_a_step(rows);
        assert!(
            dark.is_empty(),
            "{rows} rows: dark composer columns {dark:?}"
        );
        assert!(
            stray.is_empty(),
            "{rows} rows: band cells off the composer {stray:?}"
        );
        assert_eq!(retired, 0, "{rows} rows: nothing melts");
        assert_eq!(verdicts, "", "{rows} rows: no move judged");
    }
}

type WrappedTake = (Vec<(Vec<u16>, Vec<u16>)>, Vec<(u16, u16)>, u64);

fn wrapped_take(steps: bool) -> WrappedTake {
    let mut c = Claude::new();
    c.wrap = Some(72);
    c.type_str("check ");
    c.paste(URL);
    c.idle(60);
    c.type_str(" these kitties");
    let lit = |c: &Claude| (c.h.live(TEXT_ROW - 1), c.h.live(TEXT_ROW));
    let mut lives = Vec::new();
    for (i, ch) in " are a bit TOO cute.".chars().enumerate() {
        c.key(ch);
        if i % 3 == 1 {
            c.idle(20);
            if steps {
                c.tool_result();
            } else {
                c.frame();
            }
        }
        lives.push(lit(&c));
    }
    for _ in 0..15 {
        c.idle(100);
        if steps {
            c.tool_result();
        } else {
            c.frame();
        }
        lives.push(lit(&c));
    }
    assert!(c.lines().1.is_some(), "the take wrapped: {:?}", c.lines());
    let mut stray = c.stray();
    stray.extend(c.on_scroll_frames.iter().copied());
    (lives, stray, c.h.counts().1)
}

#[test]
fn a_wrapped_take_lives_the_same_life_under_tool_steps() {
    let (calm, calm_stray, calm_retired) = wrapped_take(false);
    let (stepped, stray, retired) = wrapped_take(true);
    assert!(
        calm_stray.is_empty() && calm_retired == 0,
        "fixture: the calm take is clean"
    );
    assert!(
        calm.iter().any(|(first, _)| !first.is_empty()),
        "fixture: the first line is lit"
    );
    for (i, (want, got)) in calm.iter().zip(&stepped).enumerate() {
        assert_eq!(
            got, want,
            "sample {i}: (first line, second line) live columns"
        );
    }
    assert!(stray.is_empty(), "band cells off the composer {stray:?}");
    assert_eq!(retired, 0, "nothing melts");
}

#[test]
fn a_link_pasted_with_its_line_ending_is_lit_and_so_are_the_words_after_it() {
    let mut c = Claude::new();
    c.type_str("check ");
    c.deliver_as(InsertWidth::Line(49));
    let judged = c.h.glow.admission_tally();
    c.echo(&format!("{URL}\n"));
    c.frame();
    let verdicts = c.verdicts_since(judged);
    let from = COL0 + 6;
    assert!(
        verdicts.starts_with(&format!(
            "licensed {:?}->{:?} insert; ",
            (TEXT_ROW - 1, from),
            (TEXT_ROW - 1, from + 49),
        )),
        "the link's sweep on its line, first: {verdicts}"
    );
    c.idle(60);
    let first = c.h.live(TEXT_ROW - 1);
    let dark: Vec<u16> = (COL0..from + 49)
        .filter(|col| !first.contains(col))
        .collect();
    assert!(
        dark.is_empty(),
        "dark columns of the link's line {dark:?} ({verdicts})"
    );
    c.type_str("these kitties");
    c.idle(60);
    let second = c.h.live(TEXT_ROW);
    let dark: Vec<u16> = (COL0..COL0 + 13)
        .filter(|col| !second.contains(col))
        .collect();
    assert!(
        dark.is_empty(),
        "dark columns of the words after it {dark:?}"
    );
    assert!(
        c.stray().is_empty(),
        "band cells off the composer {:?}",
        c.stray()
    );
    assert_eq!(c.h.counts().1, 0, "nothing melts");
}

fn back_to_back_steps(n: usize) -> (Vec<u16>, Vec<(u16, u16)>, u64) {
    let mut c = Claude::new();
    c.type_str("check ");
    c.paste(URL);
    c.idle(60);
    c.type_str(" these");
    c.idle(60);
    for _ in 0..n {
        c.h.now += Duration::from_millis(FRAME_MS);
        c.tool_rows(3);
    }
    c.idle(60);
    c.type_str(" kitties");
    c.idle(60);
    let mut stray = c.stray();
    stray.extend(c.on_scroll_frames.iter().copied());
    (c.dark_text(), stray, c.h.counts().1)
}

#[test]
fn back_to_back_scrolls_each_carry_their_own_rows() {
    for n in [2, 3] {
        let (dark, stray, retired) = back_to_back_steps(n);
        assert!(dark.is_empty(), "{n} steps: dark composer columns {dark:?}");
        assert!(
            stray.is_empty(),
            "{n} steps: band cells off the composer {stray:?}"
        );
        assert_eq!(retired, 0, "{n} steps: nothing melts");
    }
}

fn rest_after_a_torn_step() -> Vec<RestSample> {
    let mut c = Claude::new();
    c.type_str("check ");
    c.paste(URL);
    c.idle(120);
    c.land_tool_rows_unfinished(3);
    c.present();
    c.on_scroll_frames.extend(c.stray());
    c.h.term.process(SYNC_END.as_bytes());
    (0..40)
        .map(|_| {
            c.idle(100);
            let mut stray = c.stray();
            stray.append(&mut c.on_scroll_frames);
            (c.h.live(TEXT_ROW), stray)
        })
        .collect()
}

#[test]
fn a_torn_tool_step_returns_the_hand_with_its_line() {
    let calm = rest_after_paste(false);
    let torn = rest_after_a_torn_step();
    assert!(
        !calm[0].0.is_empty(),
        "the paste is lit after it: {:?}",
        calm[0]
    );
    for (i, (want, got)) in calm.iter().zip(&torn).enumerate() {
        assert_eq!(got, want, "{} ms into the rest", (i + 1) * 100);
    }
}

const FRESH_LINES: usize = 3;

const STEP_ROWS: usize = 4;

fn pushed_take(steps: bool) -> (Vec<u16>, Vec<(u16, u16)>, u64, String) {
    let mut c = Claude::with_transcript(FRESH_LINES, COLS);
    assert_eq!(c.row, 7, "fixture: the box starts high on the screen");
    let mut rows = vec![c.row];
    let (mut dark, mut stray) = (Vec::new(), Vec::new());
    let mut step = |c: &mut Claude, rows: &mut Vec<u16>| {
        c.idle(40);
        if steps {
            c.tool_rows(STEP_ROWS);
        } else {
            c.frame();
        }
        rows.push(c.row);
        c.idle(40);
        dark.extend(c.dark_text());
        stray.extend(c.stray());
    };
    c.type_str("check ");
    c.paste(URL);
    step(&mut c, &mut rows);
    for word in [" these", " kitties", " are", " a bit", " TOO", " cute."] {
        c.type_str(word);
        step(&mut c, &mut rows);
    }
    c.idle(60);
    if steps {
        assert_eq!(
            rows,
            [7, 11, 15, 19, 23, 26, 26, 26],
            "fixture: pushed four rows a step, then the last three and a scroll of one, then scrolled"
        );
    }
    dark.extend(c.dark_text());
    dark.sort_unstable();
    dark.dedup();
    stray.extend(c.stray());
    stray.extend(c.on_scroll_frames.iter().copied());
    let note = format!("box rows {rows:?}");
    (dark, stray, c.h.counts().1, note)
}

#[test]
fn the_owners_take_on_a_fresh_screen_with_no_tool_step_is_lit() {
    let (dark, stray, retired, note) = pushed_take(false);
    assert!(dark.is_empty(), "dark composer columns {dark:?} ({note})");
    assert!(
        stray.is_empty(),
        "band cells off the composer {stray:?} ({note})"
    );
    assert_eq!(retired, 0, "nothing melts ({note})");
}

#[test]
fn a_band_rides_a_box_the_growing_transcript_pushes_down() {
    let (dark, stray, retired, note) = pushed_take(true);
    assert!(dark.is_empty(), "dark composer columns {dark:?} ({note})");
    assert!(
        stray.is_empty(),
        "band cells off the composer {stray:?} ({note})"
    );
    assert_eq!(retired, 0, "nothing melts ({note})");
}

fn rest_after_a_push_by(step_rows: usize, pre_idle_ms: u64) -> Vec<RestSample> {
    let mut c = Claude::with_transcript(FRESH_LINES, COLS);
    c.type_str("check ");
    c.paste(URL);
    c.idle(pre_idle_ms);
    if step_rows > 0 {
        c.tool_rows(step_rows);
        assert_eq!(
            usize::from(c.row),
            7 + step_rows,
            "fixture: the step pushed the box down"
        );
    } else {
        c.frame();
    }
    (0..40)
        .map(|_| {
            c.idle(100);
            let mut stray = c.stray();
            stray.append(&mut c.on_scroll_frames);
            (c.h.live(c.row), stray)
        })
        .collect()
}

#[test]
fn a_band_pushed_down_with_its_box_lives_the_same_life_at_rest() {
    for pre_idle_ms in [120u64, 600] {
        let calm = rest_after_a_push_by(0, pre_idle_ms);
        assert!(
            !calm[0].0.is_empty(),
            "the paste is lit after {pre_idle_ms} ms idle: {:?}",
            calm[0]
        );
        for step_rows in [1usize, 2, STEP_ROWS] {
            let pushed = rest_after_a_push_by(step_rows, pre_idle_ms);
            for (i, (want, got)) in calm.iter().zip(&pushed).enumerate() {
                assert_eq!(
                    got,
                    want,
                    "step_rows={step_rows}, pre_idle={pre_idle_ms} ms, {} ms into the rest",
                    (i + 1) * 100
                );
            }
        }
    }
}

fn pushed_wrapped_take(step_rows: usize) -> WrappedTake {
    let mut c = Claude::with_transcript(FRESH_LINES, LAB_COLS);
    c.wrap = Some(140);
    c.space_wrap = true;
    c.type_str("check ");
    c.paste(URL);
    c.idle(60);
    c.type_str(
        " these kitties are a bit TOO cute. also, the rainbow cursor trail broke when I pasted ",
    );
    assert_eq!(
        (c.row, c.lines().1.as_deref()),
        (8, Some("")),
        "fixture: the Space after `pasted` wrapped the box down a row"
    );
    let lit = |c: &Claude| (c.h.live(c.row - 1), c.h.live(c.row));
    let step = |c: &mut Claude| {
        if step_rows > 0 {
            c.tool_rows(step_rows);
        } else {
            c.frame();
        }
    };
    let mut lives = vec![lit(&c)];
    for (i, ch) in "in this link here".chars().enumerate() {
        c.key(ch);
        if i % 3 == 1 {
            c.idle(20);
            step(&mut c);
        }
        lives.push(lit(&c));
    }
    for _ in 0..15 {
        c.idle(100);
        step(&mut c);
        lives.push(lit(&c));
    }
    if step_rows == STEP_ROWS {
        assert_eq!(
            c.row, TEXT_ROW,
            "fixture: the steps pushed the box to the bottom"
        );
    }
    let mut stray = c.stray();
    stray.extend(c.on_scroll_frames.iter().copied());
    (lives, stray, c.h.counts().1)
}

#[test]
fn a_wrapped_box_pushed_down_lives_the_same_life() {
    let (calm, calm_stray, calm_retired) = pushed_wrapped_take(0);
    assert!(
        calm_stray.is_empty() && calm_retired == 0,
        "fixture: the calm take is clean"
    );
    assert!(
        calm.iter().any(|(first, _)| !first.is_empty()),
        "fixture: the first line is lit"
    );
    for step_rows in [1usize, 2, STEP_ROWS] {
        let (pushed, stray, retired) = pushed_wrapped_take(step_rows);
        for (i, (want, got)) in calm.iter().zip(&pushed).enumerate() {
            assert_eq!(
                got, want,
                "step_rows={step_rows}, sample {i}: (first line, second line) live columns"
            );
        }
        assert!(
            stray.is_empty(),
            "step_rows={step_rows}: band cells off the composer {stray:?}"
        );
        assert_eq!(retired, 0, "step_rows={step_rows}: nothing melts");
    }
}

fn one_glyph_under_a_push(step_rows: usize, pre_idle_ms: u64) -> (Vec<u16>, Vec<(u16, u16)>, u64) {
    let mut c = Claude::with_transcript(FRESH_LINES, COLS);
    c.type_str("t");
    c.idle(pre_idle_ms);
    c.tool_rows(step_rows);
    assert_eq!(
        usize::from(c.row),
        7 + step_rows,
        "fixture: the step pushed the box down"
    );
    c.idle(200);
    let mut stray = c.stray();
    stray.extend(c.on_scroll_frames.iter().copied());
    (c.dark_text(), stray, c.h.counts().1)
}

#[test]
fn a_one_glyph_line_rides_a_box_pushed_down() {
    for pre_idle_ms in [300u64, 600] {
        for step_rows in [1usize, 2, STEP_ROWS] {
            let (dark, stray, retired) = one_glyph_under_a_push(step_rows, pre_idle_ms);
            assert!(
                dark.is_empty(),
                "step_rows={step_rows}, pre_idle={pre_idle_ms}: dark composer columns {dark:?}"
            );
            assert!(
                stray.is_empty(),
                "step_rows={step_rows}, pre_idle={pre_idle_ms}: band cells off the composer {stray:?}"
            );
            assert_eq!(
                retired, 0,
                "step_rows={step_rows}, pre_idle={pre_idle_ms}: nothing melts"
            );
        }
    }
}

fn push_under_a_key(step_rows: usize) -> (Vec<u16>, Vec<(u16, u16)>, u64, String) {
    let mut c = Claude::with_transcript(FRESH_LINES, COLS);
    c.type_str("check ");
    c.paste(URL);
    c.idle(60);
    c.type_str(" th");
    let judged = c.h.glow.admission_tally();
    c.press('e');
    c.h.now += Duration::from_millis(11);
    c.tool_rows(step_rows);
    assert_eq!(
        usize::from(c.row),
        7 + step_rows,
        "fixture: the step pushed the box down"
    );
    c.h.now += Duration::from_millis(10);
    c.echo("e");
    c.frame();
    c.idle(48);
    let verdicts = c.verdicts_since(judged);
    c.type_str("se kitties are a bit TOO cute.");
    c.idle(60);
    let mut stray = c.stray();
    stray.extend(c.on_scroll_frames.iter().copied());
    (c.dark_text(), stray, c.h.counts().1, verdicts)
}

#[test]
fn a_key_pressed_as_a_tool_step_pushes_the_box_keeps_its_press() {
    for step_rows in [1usize, 2, STEP_ROWS] {
        let (dark, stray, retired, verdicts) = push_under_a_key(step_rows);
        let row = u16::try_from(7 + step_rows).unwrap();
        assert!(
            dark.is_empty(),
            "step_rows={step_rows}: dark composer columns {dark:?} (verdicts: {verdicts})"
        );
        assert!(
            stray.is_empty(),
            "step_rows={step_rows}: band cells off the composer {stray:?} (verdicts: {verdicts})"
        );
        assert_eq!(
            retired, 0,
            "step_rows={step_rows}: nothing melts (verdicts: {verdicts})"
        );
        assert_eq!(
            verdicts,
            format!("licensed {:?}->{:?} key", (row, 60), (row, 61)),
            "step_rows={step_rows}: the echo is the one move judged from the press on, licensed by its key"
        );
    }
}
