// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A band that followed its text once follows it again.
//!
//! The follow pass (`rainbow_kitty::witness::Witness::follow_runs`) carries a band onto the row
//! its text moved to. A record it carried stands on a row whose neighbours it has never seen, and
//! a second move with no key between (a box relocated twice, an inline chat box pushed down by two
//! streamed rows, Claude Code's composer growing at a second newline after the first's far-row
//! read was dropped) must follow as the first did. How the witness keeps its evidence across a
//! carry, a scroll, a torn repaint and a withheld row is the `rainbow_kitty::witness` module doc.
//! Every law runs at the host seam (`trail_host::Core`).

use crate::trail_host::{Core, Opts, Theme};
use aterm_effects::rainbow_kitty::TypedClass;
use std::time::Duration;

/// The GUI's frame seam: the content-scroll seam (a frame that scrolled samples nothing), the
/// frame hold's rows (the caret row's probe, then every row the witness names), then the tick.
const SEAM: Opts = Opts {
    scroll_seam: true,
    alt_rebaseline: false,
    skip_caret: true,
    honour_sync: false,
    pane_columns: false,
};

/// `trail_host`'s frame seam on a grid of 8×16 cells. `drop_far_frames = 1` drops the next
/// frame's far-row read: only the caret's row and its `±1` (read under the first terminal lock in
/// `app_render.rs`) reach the witness, as when the second, generation-checked read loses its race
/// with a newer grid.
struct Host {
    c: Core,
}
core_deref!(Host);

impl Host {
    fn new(rows: usize, cols: usize) -> Self {
        let mut c = Core::new(rows, cols, 8, 16, Theme::Tokyo, SEAM);
        c.frame();
        Self { c }
    }

    /// Idle frames at 16 ms until `ms` have passed.
    fn idle(&mut self, ms: u64) {
        let end = self.now + Duration::from_millis(ms);
        while self.now < end {
            self.now += Duration::from_millis(16);
            self.frame();
        }
    }

    /// A key `gap` ms after the last frame: the typed hint armed at the
    /// key, the echo `bytes`, one frame.
    fn key(&mut self, ch: char, bytes: &[u8], gap: u64) {
        self.now += Duration::from_millis(gap);
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        let now = self.now;
        self.glow.note_typed_glyph(now, 1, false, class);
        self.term.process(bytes);
        self.frame();
    }

    /// Type `s` as a line editor echoes it (the glyph at the caret).
    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            let mut b = [0u8; 4];
            self.key(ch, ch.encode_utf8(&mut b).as_bytes(), 90);
        }
    }

    /// A program's own repaint, no key behind it, on the next frame.
    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }
}

/// An inline chat box (Claude Code outside fullscreen): transcript rows
/// above it, then a rule, the prompt row with `text`, a rule and a status
/// row, its top on 0-based `top` and the caret after the text. A full
/// redraw inside one synchronized update, as a streaming answer repaints.
fn draw_box(cols: usize, top: usize, text: &str, transcript: usize) -> Vec<u8> {
    let rule = "\u{2500}".repeat(cols);
    let mut s = String::from("\x1b[?2026h\x1b[?25l\x1b[H\x1b[2J");
    for i in 0..transcript {
        s.push_str(&format!(
            "\x1b[{};1H\u{25cf} streamed answer row {i} with some words in it",
            i + 1
        ));
    }
    s.push_str(&format!(
        "\x1b[{};1H{rule}\x1b[{};1H\u{276f} {text}\x1b[{};1H{rule}\x1b[{};1H  ? for shortcuts\x1b[{};{}H\x1b[?25h\x1b[?2026l",
        top + 1,
        top + 2,
        top + 3,
        top + 4,
        top + 2,
        3 + text.chars().count()
    ));
    s.into_bytes()
}

/// A line typed into an inline box, then the box re-laid by a streaming answer growing above it
/// (down) or a transcript row retracted (up), one or two rows a move, 60–300 ms between moves and
/// no key: every move carries all 33 cells onto the text's new row and retires nothing.
#[test]
fn an_inline_box_moved_by_streamed_rows_follows_every_move() {
    const COLS: usize = 100;
    let text = "fix the flaky test in the witness";
    let n = text.chars().count();
    let mut missed = Vec::new();
    for moves in [
        &[1i32, 1][..],
        &[1, 1, 1],
        &[-1, -1],
        &[1, 2],
        &[2, 1],
        &[-1, -2],
        &[1, -1],
        &[-1, 1],
        &[2, 2],
    ] {
        for gap in [60u64, 150, 300] {
            let mut h = Host::new(40, COLS);
            let mut top = 10usize;
            h.program(&draw_box(COLS, top, "", top));
            h.idle(100);
            for (i, ch) in text.chars().enumerate() {
                let bytes = format!(
                    "\x1b[?2026h\x1b[?25l\x1b[{};{}H{ch}\x1b[{};{}H\x1b[?25h\x1b[?2026l",
                    top + 2,
                    3 + i,
                    top + 2,
                    4 + i
                );
                h.key(ch, bytes.as_bytes(), 90);
            }
            h.idle(100);
            assert_eq!(
                h.live((top + 1) as u16).len(),
                n,
                "fixture: the band is under the text"
            );
            for (k, &dr) in moves.iter().enumerate() {
                top = (top as i32 + dr) as usize;
                let (f0, r0) = h.counts();
                h.program(&draw_box(COLS, top, text, top));
                h.frame();
                let (f1, r1) = h.counts();
                let got = (f1 - f0, r1 - r0, h.live((top + 1) as u16).len());
                if got != (n as u64, 0, n) {
                    missed.push((moves.to_vec(), gap, k + 1, got));
                }
                h.idle(gap);
            }
        }
    }
    assert!(
        missed.is_empty(),
        "(moves, gap ms, move, (followed, retired, live on the text's row)) that did not \
         carry all {n} cells: {missed:?}"
    );
}

/// An inline box on the screen's last four rows under streamed rows: each streamed row is written
/// over the box's top rule and the box re-written under it, the last CRLF scrolling the screen,
/// so the scroll carries the band a row up while the text ends on the same row. The follow pass
/// brings it back down, which needs the rule row seen above the text to stay seen after the scroll
/// (`Witness::translate`).
#[test]
fn an_inline_box_at_the_screen_s_bottom_keeps_its_band_under_every_streamed_row() {
    const ROWS: usize = 40;
    const COLS: usize = 100;
    let rule = "\u{2500}".repeat(COLS);
    let text = "fix the flaky test in the witness";
    let n = text.chars().count();
    let mut h = Host::new(ROWS, COLS);
    let mut s = String::new();
    for i in 0..(ROWS - 4) {
        s.push_str(&format!("\u{25cf} transcript row {i}\r\n"));
    }
    s.push_str(&format!(
        "{rule}\r\n\u{276f} \r\n{rule}\r\n  ? for shortcuts\x1b[{};3H",
        ROWS - 2
    ));
    h.program(s.as_bytes());
    h.idle(100);
    let text_row = (ROWS - 3) as u16;
    for (i, ch) in text.chars().enumerate() {
        let bytes = format!("\x1b[{};{}H{ch}", ROWS - 2, 3 + i);
        h.key(ch, bytes.as_bytes(), 90);
    }
    h.idle(100);
    assert_eq!(
        h.live(text_row).len(),
        n,
        "fixture: the band is under the text"
    );
    for k in 1..=3usize {
        let (f0, r0) = h.counts();
        h.program(
            format!(
                "\x1b[?2026h\x1b[?25l\x1b[{};1H\u{25cf} streamed row {k}\x1b[K\r\n{rule}\r\n\u{276f} {text}\x1b[K\r\n{rule}\r\n  ? for shortcuts\x1b[K\x1b[{};{}H\x1b[?25h\x1b[?2026l",
                ROWS - 3,
                ROWS - 2,
                3 + n
            )
            .as_bytes(),
        );
        h.frame();
        let (f1, r1) = h.counts();
        assert_eq!(
            (f1 - f0, r1 - r0, h.live(text_row).len()),
            (n as u64, 0, n),
            "streamed row {k}: the band is back under the text on row {text_row}"
        );
        h.idle(150);
    }
}

/// Claude Code's bottom-anchored composer on the alternate screen (the
/// shape of `tests/composer_multiline_wrap.rs`): a rule, the text rows, the
/// caret row, a rule and a status row. A composer newline (Shift+Enter)
/// repaints the box one row higher with an empty caret row, inside one
/// synchronized update.
struct Composer {
    h: Host,
    rows: usize,
    cols: usize,
    lines: Vec<String>,
    cur: String,
}

impl Composer {
    const PROMPT: &'static str = "\u{276f}\u{a0}";

    fn new(rows: usize, cols: usize) -> Self {
        let mut h = Host::new(rows, cols);
        let rule = "\u{2500}".repeat(cols);
        h.term.process(
            format!(
                "\x1b[?1049h\x1b[2J\x1b[{};1H{rule}\x1b[{};1H{}\x1b[{};1H{rule}\x1b[{};1H  auto mode on\x1b[{};3H",
                rows - 3,
                rows - 2,
                Self::PROMPT,
                rows - 1,
                rows,
                rows - 2
            )
            .as_bytes(),
        );
        h.frame();
        Self {
            h,
            rows,
            cols,
            lines: Vec::new(),
            cur: String::new(),
        }
    }

    /// The caret row, 1-based (the composer's CUP row).
    fn caret_row1(&self) -> usize {
        self.rows - 2
    }

    /// 0-based row of completed line `i`.
    fn line_row(&self, i: usize) -> u16 {
        (self.caret_row1() - 1 - self.lines.len() + i) as u16
    }

    /// One key: the glyph written at the caret (Ink's cursor-relative
    /// write), the caret parked after it.
    fn key(&mut self, ch: char) {
        let c = 3 + self.cur.chars().count();
        let row = self.caret_row1();
        let bytes = format!(
            "\x1b[?2026h\x1b[?25l\x1b[{row};{c}H{ch}\x1b[{};1H\x1b[{row};{}H\x1b[?25h\x1b[?2026l",
            self.rows,
            c + 1
        );
        self.cur.push(ch);
        self.h.key(ch, bytes.as_bytes(), 80);
    }

    /// The composer newline: the box repainted a row higher, the typed row
    /// now a completed line, an empty caret row. The host arms the newline
    /// and typed hints at the key, then the repaint lands; the frame is not
    /// run here.
    fn newline(&mut self) {
        self.lines.push(std::mem::take(&mut self.cur));
        let top = self.caret_row1() - 1 - self.lines.len();
        let rule = "\u{2500}".repeat(self.cols);
        let mut s = format!("\x1b[?2026h\x1b[?25l\x1b[{top};1H{rule}");
        for (i, l) in self.lines.iter().enumerate() {
            let pre = if i == 0 { Self::PROMPT } else { "  " };
            s.push_str(&format!("\x1b[{};1H{pre}{l}\x1b[K", top + 1 + i));
        }
        s.push_str(&format!(
            "\x1b[{};1H  \x1b[K\x1b[{};3H\x1b[?25h\x1b[?2026l",
            self.caret_row1(),
            self.caret_row1()
        ));
        self.h.now += Duration::from_millis(80);
        let now = self.h.now;
        self.h.glow.note_newline_break(now);
        self.h
            .glow
            .note_typed_glyph(now, 1, false, TypedClass::Glyph);
        self.h.term.process(s.as_bytes());
    }
}

/// A far-row read dropped on one newline's frame costs no later follow. A list typed a line at a
/// time into Claude Code's composer: on the first newline's echo frame only the caret's row and
/// its `±1` reach the witness, and the first line still follows up (to the caret's `−1`). At the
/// second newline it moves up again and follows again: the run beside the withheld row waits one
/// walk, and a carried record's unseen neighbours are no evidence against the move. Line 1 shares
/// `- ` with line 0 at the same columns; those letters must not read as holes.
#[test]
fn a_far_row_read_dropped_on_one_newline_s_frame_costs_no_later_follow() {
    let mut c = Composer::new(53, 91);
    let list = ["- fix the wrap", "- keep the band", "- test it all"];
    for (k, line) in list.iter().enumerate() {
        for ch in line.chars() {
            c.key(ch);
        }
        if k + 1 == list.len() {
            break;
        }
        // Lit cells per line before the newline: the completed lines, then
        // the caret row's — the line the newline completes.
        let caret_row = (c.caret_row1() - 1) as u16;
        let mut lit: Vec<usize> = (0..c.lines.len())
            .map(|i| c.h.live(c.line_row(i)).len())
            .collect();
        lit.push(c.h.live(caret_row).len());
        let (f0, r0) = c.h.counts();
        c.newline();
        c.h.drop_far_frames = u32::from(k == 0);
        c.h.frame();
        c.h.frame();
        let (f1, r1) = c.h.counts();
        for (i, before) in lit.iter().enumerate() {
            let now = c.h.live(c.line_row(i)).len();
            assert!(
                now >= *before,
                "newline {}: line {i} (`{}`) had {before} lit cells and has {now} on row {} — \
                 (followed +{}, retired +{})",
                k + 1,
                c.lines[i],
                c.line_row(i),
                f1 - f0,
                r1 - r0
            );
        }
        assert_eq!(r1 - r0, 0, "newline {}: nothing retired", k + 1);
    }
}

/// A row the host has a slot for is never withheld: three bands fill the bands' eight witness
/// rows and the caret sits on row 20, but `CURSOR_WITNESS_ROWS` slots hold them all. A program
/// rewriting row 7's text in place retires the band's four cells on that frame, not a frame late.
#[test]
fn a_row_the_host_has_a_slot_for_is_never_withheld() {
    let mut h = Host::new(24, 80);
    for row in [3u16, 7, 11] {
        h.program(format!("\x1b[{};1H$ ", row + 1).as_bytes());
        h.type_str("abcd");
    }
    h.program(b"\x1b[21;1H$ ");
    h.idle(64);
    let named = h.named();
    assert_eq!(
        named.len(),
        aterm_effects::rainbow_kitty::witness::WITNESS_ROWS,
        "fixture: the bands fill their rows: {named:?}"
    );
    assert!(
        !named.contains(&20),
        "fixture: the caret's row is not one of them: {named:?}"
    );
    let (_, r0) = h.counts();
    h.program(b"\x1b7\x1b[8;3Hwxyz\x1b8");
    let (_, r1) = h.counts();
    assert_eq!(
        r1 - r0,
        4,
        "row 7 rewritten in place retires its band on the frame that shows it"
    );
}

/// A torn repaint still follows: with no synchronized-update bracket, a line re-laid one row away
/// in two writes on consecutive frames (new row, then the old one's erase) stands on both rows for
/// a frame as a copy not yet a twin (`Seen::pending`). Longer tears are in
/// `tests/moves_are_followed.rs`.
#[test]
fn a_torn_repaint_that_draws_the_new_row_a_frame_before_it_erases_the_old_still_follows() {
    for (from, to) in [(5u16, 6u16), (20, 19)] {
        let mut h = Host::new(24, 80);
        h.term.process(format!("\x1b[{};1H", from + 1).as_bytes());
        h.frame();
        h.type_str("hello world");
        h.idle(200);
        let want: Vec<u16> = (0..11).collect();
        assert_eq!(
            h.live(from),
            want,
            "fixture: the band is under `hello world`"
        );
        let (f0, r0) = h.counts();
        h.program(format!("\x1b[{};1Hhello world", to + 1).as_bytes());
        h.program(format!("\x1b[{};1H\x1b[2K\x1b[{};12H", from + 1, to + 1).as_bytes());
        h.frame();
        let (f1, r1) = h.counts();
        assert_eq!(
            (f1 - f0, r1 - r0, h.live(to)),
            (11, 0, want),
            "row {from} → {to}, torn over two frames: the band follows its text"
        );
    }
}
