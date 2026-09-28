// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A LINE ERASED UNDER ITS TWIN IS NOT A MOVE — EVEN WHEN ITS KEYS ECHO
//! TOGETHER** (2026-09-24).
//!
//! The follow pass carries a band onto the row its text moved to
//! (`rk::witness::Witness::follow_runs`) on the glyphs that ARRIVED there.
//! A glyph that stood at that offset beside the record while the record's
//! own glyph stood is a TWIN (`Seen::twin`): it is neutral — neither found
//! nor a hole — and a run whose glyphs at an offset are all twins has
//! nothing that arrived there. A line killed under an identical line (`$ cd
//! ..` typed under `$ cd ..`, then Ctrl-U) is such a run, and nothing may
//! follow onto the line above: carrying the band there lit a previous
//! command, fzf's best match or vim's duplicate line, which no key wrote,
//! for about 1.3 s.
//!
//! A twin is learned two ways, and these laws pin both:
//!
//! * **AT ARMING.** A copy standing beside a record on the frame the record
//!   is armed was there first, and is a twin at once (`Seen::observe` with
//!   `at_arm`). For that the neighbour rows must be SAMPLED on the arming
//!   frame, and a line's first keys have no band of their own yet to name
//!   them. So the host names a waiting key's row and the rows one above and
//!   below it (`rk::Engine::ribbon_rows_for`), in slots of their own after
//!   the bands' rows (`CURSOR_WITNESS_ROWS`): a full band budget never drops
//!   them.
//! * **BY TIME.** A copy seen beside the standing record on every sampled
//!   frame for `TWIN_MIN` (40 ms) is a twin (`Seen::pending`).
//!
//! What the witness never saw is no evidence either way.
//!
//! The time rule alone covers a line typed at a human pace and erased
//! later: every record has stood beside the line above for `TWIN_MIN` by
//! then. The arm-time twin is what covers an erase that comes within
//! `TWIN_MIN` of the last keys' echo — a fast final bigram then Ctrl-U, a
//! paste then Ctrl-U, a whole command echoed in one frame — and those laws
//! are RED without it: `Seen::observe`'s `at_arm` arm, and the arming rows
//! named on the key's frame, are each load-bearing
//! (`a_ctrl_u_right_after_a_final_burst_under_its_twin_follows_nothing`,
//! `a_ctrl_u_right_after_a_whole_command_echoed_at_once_under_its_twin_follows_nothing`,
//! `a_ctrl_u_right_after_a_paste_under_its_twin_follows_nothing`).
//!
//! Every law runs at the HOST seam (`trail_host::Core`, the frame hold): a real
//! `aterm_core::terminal::Terminal` driven byte for byte, its rows sampled
//! as `app_render.rs`'s frame hold samples them — the caret row's probe first,
//! then every row `CursorGlow::ribbon_rows` names, read after the batch is
//! applied — and ticked through `CursorGlow::tick`. The reading is
//! `(followed, frames the twin row was lit over the next ~1.4 s)`, against
//! `(0, 0)`; each law says what 0.93.0 read. `tests/copies_are_not_moves.rs`
//! sweeps these shapes over a whole grid — keys per frame, echo lag, erase
//! key, the composed host.
//!
//! THE LIMIT THIS DOES NOT CLOSE. A line that IS identical to the one above
//! it — or differs from it in one column (`step 2:` under `step 1:`) — and
//! then genuinely moves up onto it is indistinguishable, glyph for glyph,
//! from the same line erased under an identical one: nothing at the
//! destination arrived that was not already standing there. The follow
//! pass refuses both, so such a line melts on its row instead of following
//! (the witness module doc says so, and `tests/composer_multiline_wrap.rs`'s
//! `the_limit_a_line_identical_to_the_one_above_it_or_one_column_off_melts_instead_of_following`
//! states it as a law, in the composer where it shows).

#[macro_use]
mod trail_host;

use aterm_effects::cursor_glow::InsertWidth;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::Duration;
use trail_host::{Core, LOCK_A, Theme};

const ROWS: usize = 24;
const COLS: usize = 80;

/// The host: `trail_host`'s frame-hold seam on a 24×80 grid of 8×16
/// cells, and this file's input gestures.
struct Host {
    c: Core,
}
core_deref!(Host);

impl Host {
    /// A fresh host with the caret parked at 0-based `(row, 0)` and the
    /// anchor seeded by one frame.
    fn at_row(row: u16) -> Self {
        let mut c = Core::new(ROWS, COLS, 8, 16, Theme::Tokyo, LOCK_A);
        c.term.process(format!("\x1b[{};1H", row + 1).as_bytes());
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

    /// One keypress 90 ms after the last: the typed hint, the echo, the
    /// frame.
    fn key(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(90);
        let now = self.now;
        self.glow.note_typed_cells(now, 1);
        self.term.process(bytes);
        self.frame();
    }

    /// Type `s` a key at a time, one echo per frame.
    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            let mut buf = [0u8; 4];
            self.key(ch.encode_utf8(&mut buf).as_bytes());
        }
    }

    /// Several keys 6 ms apart whose echoes land in ONE frame: two presses
    /// inside one 16 ms frame, or a line editor that echoed two keys in one
    /// write.
    fn burst(&mut self, s: &str) {
        self.now += Duration::from_millis(90);
        let mut t = self.now;
        for _ in s.chars() {
            self.glow.note_typed_cells(t, 1);
            t += Duration::from_millis(6);
        }
        self.now = t;
        self.term.process(s.as_bytes());
        self.frame();
    }

    /// A paste of `s` 90 ms after the last event: the host's insert
    /// delivery, the echo, the frame.
    fn paste(&mut self, s: &str) {
        self.now += Duration::from_millis(90);
        let now = self.now;
        let width = u16::try_from(s.chars().count()).expect("a short paste");
        self.glow
            .note_insert_delivered_from(now, now, InsertWidth::Cells(width));
        self.term.process(s.as_bytes());
        self.frame();
    }

    /// A PTY batch with no key behind it, presented on one frame.
    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }

    /// The kill key (Ctrl-U, Ctrl-W) 90 ms after the last event: the hand's
    /// kill hint, then the line editor's erase `bytes`, presented on one
    /// frame.
    fn kill(&mut self, bytes: &[u8]) {
        self.kill_after(90, bytes);
    }

    /// The kill key `gap` ms after the last event, the frames of that gap
    /// drawn at 16 ms.
    fn kill_after(&mut self, gap: u64, bytes: &[u8]) {
        let at = self.now + Duration::from_millis(gap);
        while self.now + Duration::from_millis(16) < at {
            self.now += Duration::from_millis(16);
            self.frame();
        }
        self.now = at;
        let now = self.now;
        self.glow.note_kill(now, true);
        self.term.process(bytes);
        self.frame();
    }

    /// The frames, over the next ~1.4 s (90 idle frames — longer than the
    /// 1.3 s a false follow kept the row lit), on which `row` is lit.
    fn lit_frames(&mut self, row: u16) -> usize {
        let mut n = 0;
        for _ in 0..90 {
            self.idle(16);
            n += usize::from(self.lit(row));
        }
        n
    }

    /// The verdict every law below states: nothing followed, and `row` —
    /// the twin line no key wrote — never lit.
    fn assert_never_followed_onto(&mut self, row: u16, what: &str) {
        let followed = self.followed();
        let cells = self.cells(row);
        let lit = self.lit_frames(row);
        assert_eq!(
            (followed, lit),
            (0, 0),
            "{what}: (followed, frames row {row} was lit) — nothing moved, so \
             nothing may follow onto the twin line (row {row} right after the \
             erase: {cells:?})"
        );
    }
}

/// `$ git status` on row 4, the caret on row 5 after `$ `.
fn under_git_status() -> Host {
    let mut h = Host::at_row(4);
    h.program(b"$ git status\r\n$ ");
    h
}

/// **READLINE'S REDRAW ERASES A LINE TYPED UNDER ITS TWIN, ITS FIRST TWO
/// KEYS ECHOED IN ONE FRAME.** The previous command `$ git status` stands
/// on row 4; the hand types `git status` again on row 5 — `gi` landing in
/// one frame — and readline's kill redraw (`\r$ \x1b[K`, no key hint: a
/// program's own repaint) blanks it. No band lives on row 5 when `gi`
/// echoes, so the arming rows are what show row 4 on that frame; and every
/// record has stood beside row 4 for `TWIN_MIN` by the erase. Two glyphs
/// with no twin would be a block that arrived, and the whole band would go
/// up onto the previous command. GREEN on 0.93.0.
#[test]
fn a_redraw_erasing_a_line_typed_under_its_twin_follows_nothing_when_two_keys_echo_together() {
    let mut h = under_git_status();
    h.burst("gi");
    h.type_str("t status");
    h.idle(64);
    assert_eq!(
        h.live(5),
        (2..12).collect::<Vec<u16>>(),
        "the band is under `git status`"
    );
    assert!(!h.lit(4), "row 4 is dark before the erase");
    h.program(b"\r$ \x1b[K");
    h.assert_never_followed_onto(4, "readline's redraw");
}

/// **…AND THE SAME WITH ONE KEY PER FRAME, THE CONTROL.** One record armed
/// without a twin would be one found glyph, too little evidence for a
/// move. GREEN on 0.93.0.
#[test]
fn a_redraw_erasing_a_line_typed_under_its_twin_a_key_a_frame_follows_nothing() {
    let mut h = under_git_status();
    h.type_str("git status");
    h.idle(64);
    h.program(b"\r$ \x1b[K");
    h.assert_never_followed_onto(4, "one key per frame");
}

/// **CTRL-U UNDER THE TWIN.** The same line, killed by the hand: the kill
/// hint, then `\b`×10 and `EL`. GREEN on 0.93.0.
#[test]
fn a_ctrl_u_under_its_twin_follows_nothing_when_two_keys_echo_together() {
    let mut h = under_git_status();
    h.burst("gi");
    h.type_str("t status");
    h.idle(300);
    h.kill(b"\x08\x08\x08\x08\x08\x08\x08\x08\x08\x08\x1b[K");
    h.assert_never_followed_onto(4, "Ctrl-U");
}

/// **CTRL-W: ONLY THE LAST WORD GOES.** `git status` under `git status`,
/// `gi` in one frame; Ctrl-W erases `status` and `git ` still stands on
/// row 5. The run's glyphs are half gone, the gone half stands a row up —
/// where it always stood — and neither the band of `status` nor that of
/// `git `, whose text never moved, may go up. GREEN on 0.93.0.
#[test]
fn a_ctrl_w_under_its_twin_follows_nothing_when_two_keys_echo_together() {
    let mut h = under_git_status();
    h.burst("gi");
    h.type_str("t status");
    h.idle(300);
    h.kill(b"\x08\x08\x08\x08\x08\x08\x1b[K");
    h.assert_never_followed_onto(4, "Ctrl-W");
}

/// **A FAST FINAL BIGRAM, THEN CTRL-U BEFORE `TWIN_MIN`.** `git stat` typed
/// a key a frame under `$ git status`, then `us` echoed in one frame — the
/// last two keys — and Ctrl-U 16 or 32 ms later. Every record but the last
/// two has stood beside row 4 for `TWIN_MIN`; `u` and `s` are twins only
/// because the copy stood there on the frame they were armed. Two glyphs
/// that arrived are a block, so without the arm-time twin the band is
/// carried onto the previous command. GREEN on 0.93.0; RED without
/// `Seen::observe`'s `at_arm` arm.
#[test]
fn a_ctrl_u_right_after_a_final_burst_under_its_twin_follows_nothing() {
    for gap in [16, 32] {
        let mut h = under_git_status();
        h.type_str("git stat");
        h.burst("us");
        assert_eq!(
            h.live(5),
            (2..12).collect::<Vec<u16>>(),
            "the band is under `git status`"
        );
        h.kill_after(gap, b"\x08\x08\x08\x08\x08\x08\x08\x08\x08\x08\x1b[K");
        h.assert_never_followed_onto(4, &format!("Ctrl-U {gap} ms after `us`"));
    }
}

/// **A WHOLE COMMAND ECHOED IN ONE FRAME, THEN CTRL-U BEFORE `TWIN_MIN`.**
/// `cd ..` under `$ cd ..`, all five keys echoed together (a line editor
/// that flushed them in one write), and Ctrl-U 16 or 32 ms later. No band
/// lives on row 5 on the frame they are armed — only the arming rows show
/// row 4 then — and none of them has stood beside row 4 for `TWIN_MIN` by
/// the erase. RED on 0.93.0: `(5, 80)`. RED without `Seen::observe`'s
/// `at_arm` arm, and RED with the waiting key's arming rows not named
/// (`rk::Engine::ribbon_rows_for`).
#[test]
fn a_ctrl_u_right_after_a_whole_command_echoed_at_once_under_its_twin_follows_nothing() {
    for gap in [16, 32] {
        let mut h = Host::at_row(4);
        h.program(b"$ cd ..\r\n$ ");
        h.burst("cd ..");
        assert_eq!(
            h.live(5),
            (2..7).collect::<Vec<u16>>(),
            "the band is under `cd ..`"
        );
        h.kill_after(gap, b"\x08\x08\x08\x08\x08\x1b[K");
        h.assert_never_followed_onto(4, &format!("Ctrl-U {gap} ms after `cd ..`"));
    }
}

/// **A PASTE, THEN CTRL-U BEFORE `TWIN_MIN`.** `git status` pasted under
/// `$ git status` (the host's insert delivery, one echo) and killed 16 or
/// 32 ms later: every record was armed on the paste's frame, and only the
/// copy seen on row 4 then makes them twins. A delivered insert waiting for
/// its echo names its row as a waiting key does (`CursorGlow::ribbon_rows`);
/// with it unnamed, the whole band goes up onto the previous command. RED
/// on 0.93.0: `(10, 80)`. RED without `Seen::observe`'s `at_arm` arm, and
/// RED with the waiting key's arming rows not named.
#[test]
fn a_ctrl_u_right_after_a_paste_under_its_twin_follows_nothing() {
    for gap in [16, 32] {
        let mut h = under_git_status();
        h.paste("git status");
        assert_eq!(
            h.live(5),
            (2..12).collect::<Vec<u16>>(),
            "the band is under the pasted `git status`"
        );
        h.kill_after(gap, b"\x08\x08\x08\x08\x08\x08\x08\x08\x08\x08\x1b[K");
        h.assert_never_followed_onto(4, &format!("Ctrl-U {gap} ms after the paste"));
    }
}

/// **THE SHORT LINE: `cd ..` UNDER `cd ..`.** Four armed glyphs, the first
/// two in one frame, erased 64 ms after the last key. RED on 0.93.0:
/// `(2, 80)` — two found glyphs of four armed was half, and the first two
/// had been armed with row 4 unseen.
#[test]
fn a_short_line_erased_under_its_twin_follows_nothing_when_two_keys_echo_together() {
    let mut h = Host::at_row(4);
    h.program(b"$ cd ..\r\n$ ");
    h.burst("cd");
    h.type_str(" ..");
    h.idle(64);
    h.program(b"\r$ \x1b[K");
    h.assert_never_followed_onto(4, "`cd ..`");
}

/// **A LONG COMMAND.** `cargo test -p aterm-effects` under itself, `ca` in
/// one frame: 24 records that stood beside their copy. GREEN on 0.93.0.
#[test]
fn a_long_command_erased_under_its_twin_follows_nothing_when_two_keys_echo_together() {
    let cmd = "cargo test -p aterm-effects";
    let mut h = Host::at_row(4);
    h.program(format!("$ {cmd}\r\n$ ").as_bytes());
    h.burst(&cmd[..2]);
    h.type_str(&cmd[2..]);
    h.idle(64);
    h.program(b"\r$ \x1b[K");
    h.assert_never_followed_onto(4, "`cargo test -p aterm-effects`");
}

/// **THE REMOTE SHELL: A LATE FIRST ECHO.** The keys come 90 ms apart as
/// ever, but the first key's echo is late (an SSH round trip) and arrives
/// with the second key's, on one frame; then typing goes on one echo per
/// frame. The hand is not fast here — the network put two records on one
/// frame. GREEN on 0.93.0.
#[test]
fn a_redraw_erasing_a_line_typed_under_its_twin_over_a_slow_link_follows_nothing() {
    let mut h = under_git_status();
    h.now += Duration::from_millis(90);
    let now = h.now;
    h.glow.note_typed_cells(now, 1);
    for _ in 0..5 {
        h.now += Duration::from_millis(16);
        h.frame();
    }
    h.now += Duration::from_millis(10);
    let now = h.now;
    h.glow.note_typed_cells(now, 1);
    h.now += Duration::from_millis(16);
    h.term.process(b"gi");
    h.frame();
    h.type_str("t status");
    h.idle(64);
    assert_eq!(
        h.live(5),
        (2..12).collect::<Vec<u16>>(),
        "the band is under `git status`"
    );
    h.program(b"\r$ \x1b[K");
    h.assert_never_followed_onto(4, "a late first echo");
}

/// **FZF: THE QUERY CLEARED UNDER ITS BEST MATCH** (default layout,
/// `--info=hidden`). The first list item `src/lib.rs` stands directly above
/// the prompt, fzf's pointer at column 0 and the item at column 2 — the
/// query's own columns. The hand types `src/li` (`sr` in one fzf render),
/// the best match stays `src/lib.rs` (fzf redraws the same item every key),
/// then Ctrl-U: the query row goes back to `> ` and the first item is
/// still `src/lib.rs`. The query went nowhere; the list row must never
/// light. GREEN on 0.93.0.
#[test]
fn an_fzf_query_cleared_under_its_best_match_follows_nothing() {
    let mut h = Host::at_row(21);
    // The list on rows 18..=20, the prompt on 21.
    h.program(b"\x1b[19;1H  tests/a.rs\x1b[20;1H  Cargo.toml\x1b[21;1H> src/lib.rs\x1b[22;1H> ");
    let redraw_list = "\x1b7\x1b[21;1H> src/lib.rs\x1b[K\x1b8";
    h.now += Duration::from_millis(90);
    let now = h.now;
    h.glow.note_typed_cells(now, 1);
    h.glow.note_typed_cells(now + Duration::from_millis(6), 1);
    h.now += Duration::from_millis(6);
    h.term.process(format!("sr{redraw_list}").as_bytes());
    h.frame();
    for ch in "c/li".chars() {
        h.key(format!("{ch}{redraw_list}").as_bytes());
    }
    h.idle(64);
    assert_eq!(
        h.live(21),
        (2..8).collect::<Vec<u16>>(),
        "the band is under the query"
    );
    h.program(format!("\r\x1b[2C\x1b[K{redraw_list}").as_bytes());
    h.assert_never_followed_onto(20, "fzf's Ctrl-U");
}

/// **VIM: A LINE TYPED UNDER ITS DUPLICATE, THEN UNDONE.** Row 2 holds
/// `    let total = 0;`; `o` opens a line below it (vim inserts a line with
/// `IL` and autoindent writes four blanks) and the hand types the same
/// statement on row 3 — `le` in one frame — then Esc `u`: the opened line
/// is deleted (`DL`) and the caret goes back up. Row 2 never changed. GREEN
/// on 0.93.0.
#[test]
fn a_vim_line_typed_under_its_duplicate_then_undone_follows_nothing() {
    let mut h = Host::at_row(3);
    h.program(
        b"\x1b[1;1Hfn f() -> i32 {\x1b[2;1H    let mut n = 1;\x1b[3;1H    let total = 0;\
          \x1b[4;1H}\x1b[5;1H~\x1b[6;1H~\x1b[4;1H",
    );
    h.program(b"\x1b[4;1H\x1b[L    ");
    h.burst("le");
    h.type_str("t total = 0;");
    h.idle(64);
    assert_eq!(
        h.live(3),
        (4..18).collect::<Vec<u16>>(),
        "the band is under the typed statement"
    );
    h.program(b"\x1b[4;1H\x1b[M\x1b[3;5H");
    h.assert_never_followed_onto(2, "vim's undo");
}

/// Four short bands typed a moment ago on rows 1, 4, 7 and 10 — with each
/// one's row and both landing rows they want twelve witness samples, and
/// the bands' budget is `WITNESS_ROWS`: they fill it.
fn four_older_bands() -> Host {
    let mut h = Host::at_row(0);
    for row in [1u16, 4, 7, 10] {
        h.program(format!("\x1b[{};1H", row + 1).as_bytes());
        h.type_str("ab");
    }
    let named = h.named();
    assert_eq!(
        named.len(),
        WITNESS_ROWS,
        "fixture: the older bands fill their budget: {named:?}"
    );
    h
}

/// **A FULL ROW BUDGET: THE ERASE UNDER THE TWIN STILL FOLLOWS NOTHING.**
/// Four older bands fill the bands' row budget when the hand starts a line
/// under `$ git status`, its first two keys in one frame and erased 64 ms
/// after the last; and `cd ..` echoed in one frame under `$ cd ..`, erased
/// 16 ms later. The arming rows have slots of their own
/// after the bands' (`rk::Engine::ribbon_rows_for`), so the copy on row 14
/// is seen on every arming frame. RED without `Seen::observe`'s `at_arm`
/// arm, and RED with the arming rows not named.
#[test]
fn a_full_row_budget_still_arms_a_line_s_first_keys_on_what_stood_above_them() {
    let mut h = four_older_bands();
    h.program(b"\x1b[15;1H$ git status\r\n$ ");
    h.burst("gi");
    h.type_str("t status");
    h.idle(64);
    assert_eq!(
        h.live(15),
        (2..12).collect::<Vec<u16>>(),
        "the band is under `git status`"
    );
    h.program(b"\r$ \x1b[K");
    h.assert_never_followed_onto(14, "a full row budget");
    let mut h = four_older_bands();
    h.program(b"\x1b[15;1H$ cd ..\r\n$ ");
    h.burst("cd ..");
    h.kill_after(16, b"\x08\x08\x08\x08\x08\x1b[K");
    h.assert_never_followed_onto(14, "a full row budget, `cd ..` in one frame");
}

/// **A FULL ROW BUDGET: A LINE MOVED TWO ROWS STILL CARRIES ITS BAND.**
/// `hi` is typed on row 15 with four older bands filling the budget, then
/// a program erases row 15 and redraws `hi` on row 17 (a box re-laid two
/// rows down, the caret with it): row 17 is the caret's, which the host
/// always reads, and nothing the witness saw says `hi` stood there before —
/// an unseen row is no evidence against a move. GREEN on 0.93.0.
#[test]
fn a_full_row_budget_still_lets_a_line_moved_two_rows_down_carry_its_band() {
    let mut h = four_older_bands();
    h.program(b"\x1b[16;1H");
    h.type_str("hi");
    h.idle(32);
    assert_eq!(h.live(15), vec![0, 1], "the band is under `hi`");
    h.program(b"\x1b[16;1H\x1b[2K\x1b[18;1Hhi");
    assert_eq!(h.followed(), 2, "both cells followed their text");
    assert_eq!(h.live(17), vec![0, 1], "…onto row 17");
    assert!(
        h.cells(15).is_empty(),
        "…and left row 15: {:?}",
        h.cells(15)
    );
    assert!(h.lit(17), "row 17 is lit under the text the hand typed");
}
