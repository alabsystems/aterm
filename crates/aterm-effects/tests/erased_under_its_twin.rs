// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A line erased under its twin is not a move, even when its keys echo together.
//!
//! The follow pass (`rainbow_kitty::witness::Witness::follow_runs`) carries a band onto the row
//! its text moved to on the glyphs that arrived there. A glyph that stood at that offset beside
//! the record while the record's own glyph stood is a twin (`Seen::twin`): neutral, neither found
//! nor a hole. A line killed under an identical line (`$ cd ..` under `$ cd ..`, then Ctrl-U) has
//! only twins there, so nothing may follow onto the line above. A twin is learned two ways, both
//! pinned here:
//!
//! * at arming: a copy beside a record on its arming frame is a twin at once (`Seen::observe`
//!   with `at_arm`). The line has no band yet to name its neighbour rows on that frame, so the
//!   host names a waiting key's row and its `±1` (`Engine::ribbon_rows_for`) in their own slots
//!   (`CURSOR_WITNESS_ROWS`). This covers an erase within `TWIN_MIN` of the last echo (a fast
//!   final bigram, a paste, a whole command in one frame);
//! * by time: a copy seen beside the standing record on every sampled frame for `TWIN_MIN`
//!   (40 ms) is a twin (`Seen::pending`).
//!
//! What the witness never saw is no evidence either way. Laws run at the host seam
//! (`trail_host::Core`, the frame hold), read as `(followed, frames the twin row was lit)` against
//! `(0, 0)`; `tests/copies_are_not_moves.rs` sweeps these shapes over timing and host.
//!
//! The limit: a line identical to the one above (or one column off) that genuinely moves up onto
//! it melts instead of following (`tests/composer_multiline_wrap.rs`).

use crate::trail_host::{Core, LOCK_A, Theme};
use aterm_effects::cursor_glow::InsertWidth;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::Duration;

const ROWS: usize = 24;
const COLS: usize = 80;

/// `trail_host`'s frame-hold seam on a 24×80 grid of 8×16 cells.
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

    /// Several keys 6 ms apart whose echoes land in one frame (two presses inside
    /// one 16 ms frame, or a line editor echoing two keys in one write).
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

    /// The kill key (Ctrl-U, Ctrl-W) `gap` ms after the last event, the frames
    /// of that gap drawn at 16 ms: the kill hint, then the line editor's erase
    /// `bytes`, presented on one frame.
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

    /// The frames over the next ~1.4 s (90 idle frames, longer than a false follow keeps a row
    /// lit) on which `row` is lit.
    fn lit_frames(&mut self, row: u16) -> usize {
        let mut n = 0;
        for _ in 0..90 {
            self.idle(16);
            n += usize::from(self.lit(row));
        }
        n
    }

    /// The verdict every law states: nothing followed, and `row` (the twin no key wrote) never lit.
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

/// A line killed by Ctrl-U 16 or 32 ms after its last echo, before `TWIN_MIN`,
/// under an identical line: only the copy seen on the arming frame makes its
/// records twins (`Seen::observe`'s `at_arm` arm).
///
/// * a fast final bigram: `git stat` typed a key a frame under `$ git status`,
///   then `us` echoed in one frame;
/// * a whole command echoed in one frame: `cd ..` under `$ cd ..`. No band
///   lives on row 5 when its keys are armed, so only the arming rows
///   (`rk::Engine::ribbon_rows_for`) show row 4;
/// * a paste: `git status` pasted under `$ git status` in one echo. A
///   delivered insert awaiting its echo names its row like a waiting key
///   (`CursorGlow::ribbon_rows`).
#[test]
fn a_ctrl_u_right_after_the_last_echo_under_its_twin_follows_nothing() {
    type Gesture = fn(&mut Host);
    let cases: [(&str, &[u8], Gesture, u16); 3] = [
        (
            "a final burst",
            b"$ git status\r\n$ ",
            |h| {
                h.type_str("git stat");
                h.burst("us");
            },
            12,
        ),
        (
            "a whole command at once",
            b"$ cd ..\r\n$ ",
            |h| h.burst("cd .."),
            7,
        ),
        (
            "a paste",
            b"$ git status\r\n$ ",
            |h| h.paste("git status"),
            12,
        ),
    ];
    for (what, twin, gesture, end) in cases {
        for gap in [16, 32] {
            let mut h = Host::at_row(4);
            h.program(twin);
            gesture(&mut h);
            assert_eq!(
                h.live(5),
                (2..end).collect::<Vec<u16>>(),
                "{what}: the band is under its twin"
            );
            let kill = format!("{}\x1b[K", "\x08".repeat(usize::from(end - 2)));
            h.kill_after(gap, kill.as_bytes());
            h.assert_never_followed_onto(4, &format!("Ctrl-U {gap} ms after {what}"));
        }
    }
}

/// A remote shell: the first echo is late (an SSH round trip) and lands with the second on one
/// frame, so the network, not the hand, put two records on one frame.
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

/// fzf (default layout, `--info=hidden`) with `src/lib.rs` directly above the prompt at the
/// query's columns: `src/li` typed (`sr` in one render), the item redrawn each key, then Ctrl-U.
/// The list row never lights.
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

/// vim: `o` opens a line under `    let total = 0;` (`IL`, autoindent), the same statement is typed
/// (`le` in one frame), then Esc `u` deletes it (`DL`). Row 2 never changed.
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

/// Four short bands on rows 1, 4, 7 and 10: with each one's row and both landing rows they fill
/// the bands' `WITNESS_ROWS` budget.
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

/// With four older bands filling the budget, a line erased under `$ git status` and `cd ..`
/// echoed in one frame under `$ cd ..` still follow nothing: the arming rows' own slots
/// (`Engine::ribbon_rows_for`) see row 14's copy on every arming frame.
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

/// With the budget full, a line moved two rows still carries its band: `hi` erased on row 15 and
/// redrawn on row 17 with the caret. The caret's row is always read, and an unseen row is no
/// evidence against a move.
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
