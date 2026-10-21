// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A composer redrawn under a scrolling transcript keeps its light.
//!
//! The owner (2026-10-20), typing into Claude Code while it worked: "the rainbow cursor
//! trail broke when I pasted in this link here". The real-Claude lab reproduces it with
//! Claude Code 2.1.304 running tool calls under a full screen: the paste's band, and
//! the typed band after it, jumped off the composer onto the rows above it and melted
//! there, while the text they lit stayed dark on the composer.
//!
//! The shape, from the lab's capture of that session (`aterm ctl cast`). Claude Code
//! hides the caret and draws its own; each key or paste echoes in one synchronized
//! frame that addresses the composer from the bottom row. When a tool result lands, one
//! frame rewrites the bottom-anchored region from the spinner row down: the transcript
//! lines first, then the spinner, the tip, the box and the footer, each line ended by
//! `CR LF`. The lines past the last row scroll the screen, so the box is written
//! `k` rows lower and scrolled back up to its own screen row: on screen the composer
//! never moves, but the grid records a scroll of `k` rows and the host carries every
//! mark up by `k` (`CursorGlow::note_scroll`). A tool line with its `Running…` row and
//! a blank is three rows, past the follow pass's `±1`/`±2`, so the band was left on the
//! tip row and melted there.
//!
//! The host is `trail_host`'s GUI-faithful frame seam: the scroll is applied first and
//! fences the row probe, then the frame hold samples the caret's row and every row the
//! witness names, on the scroll's frame too (`ScrollChange::translated_only`): the band
//! is back on the composer on the very frame the tool result is drawn, not one frame
//! later after a flash on the tip row. The take is the owner's: a typed run, the URL pasted (the
//! delivered-insert licence of `App::input_paste`, priced at its 49 cells), a typed run;
//! a tool result lands after the paste and again inside the second run. The negative
//! control: the same take with no tool result is lit either way, so what is judged is
//! the scroll.
//!
//! A tool step can also land between a key's press and its echo. The scroll carried
//! the caret's mirror up with the band, and the box redrawn in place leaves the caret on
//! its screen row: read as a move of the scroll's rows, it bought the key's press, and
//! the echo after it found none and stayed dark. The hand's seam reads a scroll's frame
//! in the line's own frame, so the caret did not move and the press waits for its echo.
//! A paste whose echo is drawn in the tool result's own frame was read the same way, a
//! move across the scroll's rows that no insert licence pays for, and the whole link
//! stayed dark: the insert's width proves its line returned, and its sweep is lit.

use std::time::Duration;

use aterm_core::terminal::{ContentScrollDelta, ContentScrollState};
use aterm_effects::cursor_glow::{AdmissionTally, CURSOR_WITNESS_ROWS, InsertWidth};
use aterm_effects::host::{hidden_caret_withheld, motion_caret, motion_print_anchor};
use aterm_effects::rainbow_kitty::TypedClass;

use crate::trail_host::{Core, Opts, Theme};

const ROWS: usize = 30;
const COLS: usize = 120;
/// The lab's window (Claude Code 2.1.305 at 144×50, 2026-10-20): the owner's
/// sentence word-wraps after `pasted`, its Space on the composer's last
/// column.
const LAB_COLS: usize = 144;
/// The composer's text row: the box sits on the four rows above the bottom.
const TEXT_ROW: u16 = (ROWS - 4) as u16;
/// The grid column of the first text glyph: `❯` + NBSP before it.
const COL0: u16 = 2;
/// The owner's link.
const URL: &str = "https://claude.ai/artifact/XB78QX8U9sp935PNjq8FpB";
/// The recorded cadence: about 115 ms between keys.
const KEY_MS: u64 = 115;
const FRAME_MS: u64 = 16;
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

/// Claude Code under a transcript, on the trail host.
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
    /// Claude Code under a full transcript: the box on the screen's bottom rows.
    fn new() -> Self {
        let c = Self::with_transcript(ROWS + 10, COLS);
        assert_eq!(c.row, TEXT_ROW, "fixture: the box is on the bottom rows");
        c
    }

    /// A fresh session's screen, `cols` wide: `lines` transcript rows above
    /// the box, which tool steps push down until it reaches the bottom.
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

    /// The composer's text as drawn: its first line, and its second when it
    /// wraps or holds a line break.
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

    /// The caret's column, after the text on the caret's line (the second
    /// when the text wraps, indented as the first).
    fn caret_col(&self) -> u16 {
        let (first, second) = self.lines();
        COL0 + second.unwrap_or(first).chars().count() as u16
    }

    /// The region's rows: one more when the composer holds two lines.
    fn region_rows(&self) -> usize {
        if self.lines().1.is_some() { 9 } else { 8 }
    }

    /// The bottom-anchored region from the spinner row down, each row erased to
    /// its end and ended by `CR LF` but the last: spinner, tip, blank, rule, the
    /// composer with Claude Code's own drawn caret, rule, footer, blank.
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

    /// The GUI's frame (`app_render.rs`): the content-scroll seam first (a
    /// scroll fences the row probe), the caret row's probe and every row the
    /// witness names (the witness's rows on a scroll's frame too), the echo
    /// anchor and the drawn-caret bit, then the tick with the motion caret (a
    /// hidden caret on a finished frame whose row draws ink is the caret,
    /// `aterm_effects::host::motion_caret`).
    fn frame(&mut self) {
        if self.h.term.sync_open_dirty() {
            return;
        }
        self.present();
    }

    /// [`Self::frame`] whatever the synchronized update's state: the GUI's
    /// hold capped, a frame the program has not finished presented as it is.
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

    /// One echo in Claude Code's per-key shape: from the caret down to the
    /// bottom row, back up to the composer, the new text and the drawn caret,
    /// down again, and the caret put after the text.
    /// The echo that wraps the text grows the box a row: the region redrawn
    /// from the spinner row, its last row past the bottom scrolling the screen
    /// one row, the first line on the row above the caret's (on a screen not
    /// full yet the box grows down instead, the caret a row lower).
    fn echo(&mut self, s: &str) {
        let c = self.caret_col();
        let grows = self.region_rows();
        self.text.push_str(s);
        if self.region_rows() > grows && self.space_wrap {
            // The lab's cast: the old drawn caret erased, the second line's
            // drawn caret, the rule and footer a row lower, a new blank row
            // under them (past the bottom of a full screen, a scroll of one).
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

    /// A key's press, `KEY_MS` after the last event: the window's stamp
    /// (`note_typed_expected`, the glyph riding it), 2 ms before anything else.
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

    /// A key: the press, and its echo 2 ms later.
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

    /// `App::input_paste`: the gesture stamped and revoked at dispatch, the
    /// delivered-insert licence armed from the writer's receipt (priced by
    /// `Terminal::paste_insert_cells`), then the echo.
    fn paste(&mut self, s: &str) {
        self.deliver(s);
        self.echo(s);
        self.frame();
    }

    /// [`Self::paste`] up to its echo: the licence armed, 2 ms gone.
    fn deliver(&mut self, s: &str) {
        let cells = u16::try_from(s.chars().count()).expect("a short paste");
        self.deliver_as(InsertWidth::Cells(cells));
    }

    /// [`Self::deliver`] of a paste the host priced `width`.
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

    /// A tool result lands: one frame rewrites the region from the spinner row
    /// down with the result's three transcript rows first, and the rows past the
    /// bottom scroll the screen three rows, the composer back on its screen row
    /// (on a screen not full yet, the box pushed down instead).
    fn tool_result(&mut self) {
        self.tool_rows(3);
    }

    /// The frame of a tool step that adds `rows` transcript rows above the
    /// spinner (the call's line, its `Running…` row and a blank are three; its
    /// result in place of the `Running…` row is one), scrolling the screen
    /// that many rows, or on a screen not full yet pushing the box down as far
    /// as the bottom and scrolling the rest; its frame drawn, and the band
    /// cells it left off the composer recorded.
    fn tool_rows(&mut self, rows: usize) {
        self.land_tool_rows(rows);
        self.frame();
        let off = self.stray();
        self.on_scroll_frames.extend(off);
    }

    /// [`Self::tool_rows`]' bytes, processed with no frame drawn.
    fn land_tool_rows(&mut self, rows: usize) {
        self.land_tool_rows_unfinished(rows);
        self.h.term.process(SYNC_END.as_bytes());
    }

    /// [`Self::land_tool_rows`] up to the synchronized update's end: every row
    /// drawn and the caret put back on the composer, the update still open.
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

    /// The admission verdicts judged since the tally `judged` was read:
    /// `reason origin->target licence`, oldest first.
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

    /// The composer's columns that hold text, and of those the ones with no
    /// resident ribbon cell on the composer row (on the first line too, a row
    /// up, when the text wraps).
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

    /// Ribbon cells on the rows above the composer, where no key was typed,
    /// leaving ones included (a band melting where a scroll left it):
    /// `(row, col)`.
    fn stray(&self) -> Vec<(u16, u16)> {
        let top = self.row - u16::from(self.lines().1.is_some());
        (0..top)
            .flat_map(|r| self.h.cells(r).into_iter().map(move |(c, _)| (r, c)))
            .collect()
    }
}

/// What a take left: the composer's text columns with no band, the band
/// cells off the composer (read after each scroll and at the end), and the
/// cells retired (`ribbon_retired=`).
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

/// The negative control: with no tool result the take is lit from the first
/// glyph to the caret and nothing strays.
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

/// The owner's take under a streaming transcript: the pasted link's band and
/// the typed band after it stay on the composer through each tool result's
/// three-row scroll, on the scroll's own frame and after, nothing is left on
/// the rows the scroll carried them to, and nothing melts.
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

/// One sample of the band at rest: the composer row's live columns, and every
/// live cell off it.
type RestSample = (Vec<u16>, Vec<(u16, u16)>);

/// The band's life over a rest after the paste, sampled every 100 ms for four
/// seconds.
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

/// The hand returns with its line: the scroll carried the caret's mirror up
/// with the band, and the box redrawn in place carries both back, so the band
/// is not taken for text that left the caret's row (which would flow at once)
/// and rests, holds and leaves exactly as it does with no scroll under it.
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

/// The owner's take with a tool step landing between one key's press and its
/// echo: what it left (as [`take`]) and the admission verdicts from the press
/// on.
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

/// A key pressed as a tool step lands keeps its press for its echo: the box
/// redrawn in place under the step's scrolls leaves the caret on its screen
/// row, which the hand's seam reads in its line's own frame (no move), so the
/// press is not spent on the rows the scroll carried the caret's mirror, and
/// the echo is lit like every other key's. The lab's take (2026-10-20) left
/// the `e` of "these" dark this way, a gap in the band.
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

/// The owner's link pasted after `typed` with its echo drawn in a tool result's
/// frame (the result's three rows scroll the screen in the same synchronized
/// update the box is redrawn in, the link in it), then the typed run after it:
/// what it left (as [`take`]) and the paste's verdict.
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

/// A paste whose echo lands in a tool result's frame is lit: the box redrawn in
/// place under the result's scroll returned with the caret's line, which the
/// insert's own width proves (the caret back on its screen row exactly the
/// link's 49 cells on), in an empty composer too, with no typed glyph to follow.
/// The echo is judged in the line's own frame, the same-row sweep its insert
/// licence pays for. The lab's take (2026-10-20) left the whole link dark this
/// way.
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

/// One glyph typed into the composer, at rest, then a tool step of `rows` rows:
/// what it left (as [`take`]) and the verdicts the step's frames drew.
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

/// A line of one typed glyph returns with the box redrawn in place under a
/// tool step's scroll: the follow pass alone wants two of a run's records to
/// carry it, and the hand's seam carries the hand's own run on one, with the
/// caret standing on its screen row as the second witness, key or no key. The
/// lab's take (2026-10-20) melted the `th` that began the composer's second
/// line, each the line's only glyph when a step landed. The scroll's frame
/// judges no move: the caret did not move.
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

/// A wrapped take's record: each sample's live columns on the composer's first
/// and second lines, the band cells that strayed off the composer, and the
/// ribbons retired.
type WrappedTake = (Vec<(Vec<u16>, Vec<u16>)>, Vec<(u16, u16)>, u64);

/// The owner's take on a composer that wraps (the box grows a row as the text
/// passes its first line, the input going on on a second line under it), every
/// key's composer rows sampled, and every 100 ms of the rest after it: with
/// `steps`, a tool step lands after every third key and at each rest sample, a
/// frame drawn at the same instant either way. The second line begins `e ` on the
/// step after the wrap: a glyph and the blank after it.
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

/// A wrapped composer's two lines live the same lives under tool steps as with
/// none: the first line's band flows once the hand has gone down to the second
/// (the fold), at the same instant, and the second line is lit from its first
/// glyph, which the step after the wrap found with only a blank after it (the
/// lab's take, 2026-10-20, melted the `th` that began it). Nothing strays and
/// nothing melts.
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

/// The owner's link pasted with its line ending (copied from a line of text, so
/// the host prices one line and its break, `InsertWidth::Line`). Claude
/// Code puts the break in the composer: the echo grows the box a row, the link
/// on the first line and the caret on the second, in one frame that scrolls the
/// full screen a row. The link is lit as the insert's sweep on its line, the
/// rest of the move to the new line is judged as any other, and the owner's
/// words typed after it light the second line. The review's lab (2026-10-20) left the link dark this
/// way: the body was priced as a multi-line one of unknown width, and its echo
/// crossed a row.
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

/// A streamed tool result that adds its rows on every frame: `n` tool steps of
/// three rows on back-to-back frames, no frame between them that only walks
/// (each scroll's own frame skips the walk). What it left, as [`take`].
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

/// Scrolls on back-to-back frames each carry the band their own rows: the
/// follow pass that searched one scroll's carry ends it, so the next frame's
/// scroll is searched at its own three rows, not at the six or nine the two or
/// three of them add up to, and the box redrawn in place keeps its band through
/// every one of them.
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

/// The band at rest after the paste, sampled as [`rest_after_paste`] does, with
/// a tool step whose frame is presented before its synchronized update ends
/// (the GUI's hold capped): the hidden caret is withheld on that frame, so no
/// key and no caret says where the hand is, and the update's end is drawn a
/// frame later.
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

/// A tool step presented torn, its caret withheld, still returns the hand with
/// its line: the box was redrawn in place before the update's end, and on the
/// scroll's own frame the follow pass names the hand's line back on the caret's
/// screen row, past the follow offsets, with no caret to say so. The band rests,
/// holds and leaves exactly as with no step under it, and nothing is left on
/// the rows the scroll carried it to.
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

/// The transcript rows above the box on the take's fresh screen: the composer
/// starts on row 7, and four-row tool steps push it to the bottom in five.
const FRESH_LINES: usize = 3;

/// The lab's tool step (Claude Code 2.1.305, 2026-10-20): the call's line, its
/// result's two rows and a blank.
const STEP_ROWS: usize = 4;

/// The owner's take on a fresh session's screen, the transcript not yet down to
/// the box: with `steps`, a tool step after the paste and after each word, each
/// pushing the box down [`STEP_ROWS`] rows with no scroll until it reaches the
/// bottom, then one that pushes it the last three rows and scrolls one, then
/// steps that scroll it in place. What it left, as [`take`], the composer read
/// on whatever row the box stands on; without, a frame at the same instants.
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

/// The negative control: on the fresh screen with no tool step the take is lit
/// from the first glyph to the caret, and nothing strays or melts.
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

/// The owner's take on a fresh session's screen, the box pushed down four rows
/// by each tool step while the transcript has not filled the screen (past the
/// follow pass's `±1`/`±2`, and no scroll to say how far), then pushed the last
/// rows under a scroll, then scrolled in place: the band rides the box all the
/// way down, nothing is left on the rows it stood on, and nothing melts. The
/// real-Claude lab (2026-10-20, Claude Code 2.1.305) melted the whole band
/// here, the pasted link first (`ribbon_retired=55` at the paste, 70 after).
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

/// The band's life over a rest after the paste on the fresh screen, sampled as
/// [`rest_after_paste`] does on whatever row the box stands on, with or without
/// one tool step pushing the box down [`STEP_ROWS`] rows.
fn rest_after_a_push(push: bool) -> Vec<RestSample> {
    let mut c = Claude::with_transcript(FRESH_LINES, COLS);
    c.type_str("check ");
    c.paste(URL);
    c.idle(120);
    if push {
        c.tool_rows(STEP_ROWS);
        assert_eq!(c.row, 11, "fixture: the step pushed the box down");
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

/// A band pushed down with its box lives the same life at rest as one left in
/// place: it is still the hand's band on the hand's line, so it rests, holds
/// and leaves on the same clocks, and nothing is left where it stood.
#[test]
fn a_band_pushed_down_with_its_box_lives_the_same_life_at_rest() {
    let calm = rest_after_a_push(false);
    let pushed = rest_after_a_push(true);
    assert!(
        !calm[0].0.is_empty(),
        "the paste is lit after it: {:?}",
        calm[0]
    );
    for (i, (want, got)) in calm.iter().zip(&pushed).enumerate() {
        assert_eq!(got, want, "{} ms into the rest", (i + 1) * 100);
    }
}

/// The owner's take on the lab's fresh screen ([`LAB_COLS`] wide), the
/// composer word-wrapping after `pasted` as Claude Code wraps it, the box
/// growing down a row: then the rest of the sentence on the second line with,
/// with `steps`, a four-row tool step after every third key and at each rest
/// sample, pushing the two-line box down until it reaches the bottom, then
/// scrolling it in place. Each sample reads the composer's two lines on
/// whatever rows the box stands on; without `steps`, a frame at the same
/// instants.
fn pushed_wrapped_take(steps: bool) -> WrappedTake {
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
        if steps {
            c.tool_rows(STEP_ROWS);
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
    if steps {
        assert_eq!(
            c.row, TEXT_ROW,
            "fixture: the steps pushed the box to the bottom"
        );
    }
    let mut stray = c.stray();
    stray.extend(c.on_scroll_frames.iter().copied());
    (lives, stray, c.h.counts().1)
}

/// The owner's two-line composer pushed down by four-row tool steps on the lab's
/// fresh screen, then scrolled in place once it reaches the bottom, lives the same
/// lives as with no step: both lines ride the box down (the first line a row above
/// the caret, the caret's carry naming its row), the first line's band folding
/// away and the second's lit on the same clocks, nothing strays and nothing melts.
#[test]
fn a_wrapped_box_pushed_down_lives_the_same_life() {
    let (calm, calm_stray, calm_retired) = pushed_wrapped_take(false);
    let (pushed, stray, retired) = pushed_wrapped_take(true);
    assert!(
        calm_stray.is_empty() && calm_retired == 0,
        "fixture: the calm take is clean"
    );
    assert!(
        calm.iter().any(|(first, _)| !first.is_empty()),
        "fixture: the first line is lit"
    );
    for (i, (want, got)) in calm.iter().zip(&pushed).enumerate() {
        assert_eq!(
            got, want,
            "sample {i}: (first line, second line) live columns"
        );
    }
    assert!(stray.is_empty(), "band cells off the composer {stray:?}");
    assert_eq!(retired, 0, "nothing melts");
}

/// One glyph typed into the composer on the fresh screen, at rest, then a tool
/// step pushing the box down [`STEP_ROWS`] rows: what it left (as [`take`]).
fn one_glyph_under_a_push() -> (Vec<u16>, Vec<(u16, u16)>, u64) {
    let mut c = Claude::with_transcript(FRESH_LINES, COLS);
    c.type_str("t");
    c.idle(300);
    c.tool_rows(STEP_ROWS);
    assert_eq!(c.row, 11, "fixture: the step pushed the box down");
    c.idle(200);
    let mut stray = c.stray();
    stray.extend(c.on_scroll_frames.iter().copied());
    (c.dark_text(), stray, c.h.counts().1)
}

/// A line of one typed glyph rides a box pushed down past the follow offsets:
/// the follow pass alone wants two of a run's records to carry it, and the
/// hand's seam carries the hand's own run on one, with the caret standing on
/// the box's new row as the second witness, key or no key.
#[test]
fn a_one_glyph_line_rides_a_box_pushed_down() {
    let (dark, stray, retired) = one_glyph_under_a_push();
    assert!(dark.is_empty(), "dark composer columns {dark:?}");
    assert!(stray.is_empty(), "band cells off the composer {stray:?}");
    assert_eq!(retired, 0, "nothing melts");
}

/// The owner's take on the fresh screen with a tool step pushing the box down
/// between one key's press and its echo: what it left (as [`take`]) and the
/// admission verdicts from the press on.
fn push_under_a_key() -> (Vec<u16>, Vec<(u16, u16)>, u64, String) {
    let mut c = Claude::with_transcript(FRESH_LINES, COLS);
    c.type_str("check ");
    c.paste(URL);
    c.idle(60);
    c.type_str(" th");
    let judged = c.h.glow.admission_tally();
    c.press('e');
    c.h.now += Duration::from_millis(11);
    c.tool_rows(STEP_ROWS);
    assert_eq!(c.row, 11, "fixture: the step pushed the box down");
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

/// A key pressed as a tool step pushes the box down keeps its press for its
/// echo: the hand rides its line to the box's new row, so the press is not
/// spent on the rows the box moved, and the echo is lit like every other key's.
#[test]
fn a_key_pressed_as_a_tool_step_pushes_the_box_keeps_its_press() {
    let (dark, stray, retired, verdicts) = push_under_a_key();
    assert!(
        dark.is_empty(),
        "dark composer columns {dark:?} (verdicts: {verdicts})"
    );
    assert!(
        stray.is_empty(),
        "band cells off the composer {stray:?} (verdicts: {verdicts})"
    );
    assert_eq!(retired, 0, "nothing melts (verdicts: {verdicts})");
    assert_eq!(
        verdicts,
        format!("licensed {:?}->{:?} key", (11, 60), (11, 61)),
        "the echo is the one move judged from the press on, licensed by its key"
    );
}
