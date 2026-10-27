// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that a line erased under an identical line is never treated as a move by the
//! rainbow kitty follow pass, whether its twin was observed at arming or over `TWIN_MIN`.

use crate::trail_host::{Core, LOCK_A, Theme, on_every_ground};
use aterm_effects::cursor_glow::InsertWidth;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::Duration;

const ROWS: usize = 24;
const COLS: usize = 80;

struct Host {
    c: Core,
}
core_deref!(Host);

impl Host {
    fn at_row(row: u16) -> Self {
        let mut c = Core::new(ROWS, COLS, 8, 16, Theme::Tokyo, LOCK_A);
        c.term.process(format!("\x1b[{};1H", row + 1).as_bytes());
        c.frame();
        Self { c }
    }

    fn idle(&mut self, ms: u64) {
        let end = self.now + Duration::from_millis(ms);
        while self.now < end {
            self.now += Duration::from_millis(16);
            self.frame();
        }
    }

    fn key(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(90);
        let now = self.now;
        self.glow.note_typed_cells(now, 1);
        self.term.process(bytes);
        self.frame();
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            let mut buf = [0u8; 4];
            self.key(ch.encode_utf8(&mut buf).as_bytes());
        }
    }

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

    fn paste(&mut self, s: &str) {
        self.now += Duration::from_millis(90);
        let now = self.now;
        let width = u16::try_from(s.chars().count()).expect("a short paste");
        self.glow
            .note_insert_delivered_from(now, now, InsertWidth::Cells(width));
        self.term.process(s.as_bytes());
        self.frame();
    }

    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }

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

    fn lit_frames(&mut self, row: u16) -> usize {
        let mut n = 0;
        for _ in 0..90 {
            self.idle(16);
            n += usize::from(self.lit(row));
        }
        n
    }

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

fn under_git_status() -> Host {
    let mut h = Host::at_row(4);
    h.program(b"$ git status\r\n$ ");
    h
}

#[test]
fn a_ctrl_u_right_after_the_last_echo_under_its_twin_follows_nothing() {
    on_every_ground(|| {
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
    });
}

#[test]
fn a_redraw_erasing_a_line_typed_under_its_twin_over_a_slow_link_follows_nothing() {
    on_every_ground(|| {
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
    });
}

#[test]
fn an_fzf_query_cleared_under_its_best_match_follows_nothing() {
    on_every_ground(|| {
        let mut h = Host::at_row(21);
        h.program(
            b"\x1b[19;1H  tests/a.rs\x1b[20;1H  Cargo.toml\x1b[21;1H> src/lib.rs\x1b[22;1H> ",
        );
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
    });
}

#[test]
fn a_vim_line_typed_under_its_duplicate_then_undone_follows_nothing() {
    on_every_ground(|| {
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
    });
}

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

#[test]
fn a_full_row_budget_still_arms_a_line_s_first_keys_on_what_stood_above_them() {
    on_every_ground(|| {
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
    });
}

#[test]
fn a_full_row_budget_still_lets_a_line_moved_two_rows_down_carry_its_band() {
    on_every_ground(|| {
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
    });
}
