// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A BAND THAT FOLLOWED ITS TEXT ONCE FOLLOWS IT AGAIN** (2026-09-25).
//!
//! The follow pass carries a band onto the row its text moved to
//! (`rk::witness::Witness::follow_runs`) and counts a glyph found there only
//! if it ARRIVED. A record the pass carried stands on a row whose neighbours
//! it has never seen, and a SECOND move with no key between — a box
//! relocated twice, an inline chat box pushed down by two streamed rows,
//! Claude Code's composer growing at a second newline after the far-row
//! read of the first was dropped — must follow as the first did, not melt
//! under text standing right there.
//!
//! The witness's evidence (`rk::witness`, its module doc): a carried record
//! FORGETS what it saw — its new row's neighbours are not the old ones —
//! and starts over as a record just armed, and nothing it did not see is
//! evidence against a move: requiring a frame to show the destination empty
//! first melted a move on the very next frame, or two rows away
//! (`tests/moves_are_followed.rs`). A scroll moves every row alike, so it
//! keeps what was seen (`Witness::translate`). A copy beside a line is a
//! twin only once it has stood there `TWIN_MIN`, so a program that draws a
//! line's new row up to three frames before it erases the old — a torn
//! repaint, no synchronized-update bracket — still follows. And a run beside
//! a row the host was asked for and did not deliver waits one walk for its
//! verdict (`Witness::find_deferred_runs`), so a line moved onto that row
//! follows on the next frame.
//!
//! Every law runs at the HOST seam, GUI-faithfully (`trail_host::Core`: the
//! content-scroll seam, then LOCK A's rows, then the tick; a frame that
//! scrolled samples nothing). Each names its reading on 0.93.0, and the
//! mechanism that holds it.

#[macro_use]
mod trail_host;

use aterm_effects::rainbow_kitty::TypedClass;
use std::time::Duration;
use trail_host::{Core, Opts, Theme};

/// The GUI's frame seam (`trail_host::Core`): the content-scroll seam (a
/// frame that scrolled samples nothing), LOCK A's rows — the caret row's
/// probe, then every row the witness names — then the tick.
const SEAM: Opts = Opts {
    scroll_seam: true,
    alt_rebaseline: false,
    skip_caret: true,
    honour_sync: false,
    pane_columns: false,
};

/// The host: `trail_host`'s frame seam on a grid of 8×16 cells, and this
/// file's input gestures. `drop_far_frames = 1` drops the NEXT frame's
/// far-row read: only the caret's row and its `±1`, which `app_render.rs`
/// captures under its first terminal lock, reach the witness — the second,
/// generation-checked read lost its race with a newer grid.
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

/// **A LINE RELOCATED TWICE, NO KEY BETWEEN, FOLLOWS BOTH TIMES** — the
/// plain shape of `tests/abandoned_ribbon.rs`'s relocation law, twice:
/// `hello world` typed, then a program clears the screen and redraws the
/// line one row lower (or higher), and 150 ms later — still no key — one
/// row farther. The second move is carried as the first was: the carried
/// records start over, and nothing they have not seen yet counts against
/// the move. Were a carried record's unseen neighbours read as "not
/// arrived", the second move would read `(followed +0, retired +11)` in
/// both directions, the band melted under its own text. GREEN on 0.93.0.
#[test]
fn a_line_relocated_twice_without_a_key_follows_both_times() {
    for (from, to) in [(5u16, [6u16, 7]), (20, [19, 18])] {
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
        for (k, row) in to.into_iter().enumerate() {
            let (f0, r0) = h.counts();
            h.program(format!("\x1b[2J\x1b[{};1Hhello world", row + 1).as_bytes());
            h.frame();
            let (f1, r1) = h.counts();
            assert_eq!(
                (f1 - f0, r1 - r0, h.live(row)),
                (11, 0, want.clone()),
                "move {} from row {from}: the band follows its text onto row {row}, nothing retired",
                k + 1
            );
            h.idle(150);
        }
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

/// **AN INLINE BOX MOVED BY STREAMED ROWS FOLLOWS EVERY MOVE** — the
/// relocation sweep: `fix the flaky test in the witness` typed into an
/// inline box, then the box re-laid by a streaming answer growing above it
/// (down) or a transcript row retracted (up), one or two rows a move, with
/// 60, 150 or 300 ms between the moves and no key. Every move carries all
/// 33 cells onto the text's new row and retires nothing. Read fail-closed,
/// a carried record's unseen neighbours refuse every second move in the
/// direction of the first (or farther): 21 of these 27 cases,
/// `(followed +0, retired +33)`. GREEN on 0.93.0.
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

/// **AN INLINE BOX AT THE SCREEN'S BOTTOM, UNDER STREAMED ROWS.** The box
/// sits on the last four rows; each streamed row is written where the
/// box's top rule stood and the box re-written under it, the last CRLF
/// scrolling the screen: the box's text ends on the same screen row while
/// the scroll carried its band a row UP. The follow pass brings it back
/// down on the next frame — which it can only do if the rule row the text
/// was seen above still counts as seen after the scroll
/// (`Witness::translate` keeps the evidence: the whole screen moved). GREEN
/// on 0.93.0. RED with a scroll that FORGETS the evidence, from push 1; RED
/// with a carried record's unseen neighbours read fail-closed, from push 2
/// `(followed +0, retired +33, 0 lit)`.
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
/// repaints the box one row HIGHER with an empty caret row, inside one
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

/// **A FAR-ROW READ DROPPED ON ONE NEWLINE'S FRAME COSTS NO LATER FOLLOW.**
/// Claude Code's composer, a short list typed a line at a time with
/// Shift+Enter between. On the first newline's echo frame the GUI's second,
/// generation-checked far-row read is dropped (a PTY chunk landed between
/// its two locks), so only the caret's row and its `±1` reach the witness;
/// the first line still follows up (its destination is the caret's `−1`).
/// At the SECOND newline that line moves up again, and must follow again.
/// The run beside the withheld row waits one walk for its verdict, and a
/// carried record's unseen neighbours are no evidence against the second
/// move. RED on 0.93.0 at the line the neutral twin fixed: line 1 shares
/// `- ` with line 0 at the same columns, and those letters, read as holes,
/// split its block (`(followed +14, retired +15)`).
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

/// **A ROW THE HOST HAS A SLOT FOR IS NEVER WITHHELD.** Three typed bands
/// on rows 3, 7 and 11 fill the bands' eight rows of the witness's list,
/// and the caret is parked on row 20, a row no band lives on. The host
/// reads the caret's row first, then every row of the list: its
/// `CURSOR_WITNESS_ROWS` slots hold them all, so no row of the list is
/// "asked for and not delivered", and nothing is deferred. A program then
/// rewrites row 7's text in place — other glyphs at the band's columns —
/// and the band's four cells retire on that frame. With the caret's row
/// taking one of only eight slots, the list's last row got none, read as
/// withheld on every frame, and the rewrite beside it retired a frame
/// late. GREEN on 0.93.0, which deferred nothing.
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

/// **A TORN REPAINT STILL FOLLOWS.** A program with no synchronized-update
/// bracket re-lays a line one row away in two PTY writes the host presents
/// on consecutive frames: first the line on its new row, then the erase of
/// its old one. On the first frame the line stands on BOTH rows — one frame
/// of a copy beside it, which is not yet a twin (`Seen::pending`: it has not
/// stood `TWIN_MIN`) — and on the second its text has moved. The band
/// follows, down (a box pushed by a row above it) and up (a bottom-anchored
/// box growing). GREEN on 0.93.0 (it never looked again after arming); RED
/// if one frame of a copy made a twin. Tears across two and three frames
/// are `tests/moves_are_followed.rs`'s
/// (`a_torn_repaint_across_up_to_three_frames_follows`).
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
