// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Split-pane scroll copy conformance for `rainbow_kitty::witness::Witness::follow_runs`.

use crate::trail_host::{Core, Opts, Theme};
use std::time::Duration;

const ROWS: usize = 24;
const COLS: usize = 80;

const SEAM: Opts = Opts {
    scroll_seam: true,
    alt_rebaseline: true,
    skip_caret: true,
    honour_sync: false,
    pane_columns: false,
};

struct Host {
    c: Core,
}
core_deref!(Host);

impl Host {
    fn in_pane(pane_rows: usize, row: u16) -> Self {
        let mut c = Core::new(pane_rows, COLS, 8, 16, Theme::Tokyo, SEAM);
        c.g.rows = ROWS;
        c.g.win_h = u16::try_from(ROWS * 16).expect("a small window");
        c.glow.note_pane_rows(0, pane_rows);
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

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.now += Duration::from_millis(90);
            let now = self.now;
            self.glow.note_typed_cells(now, 1);
            let mut b = [0u8; 4];
            self.term.process(ch.encode_utf8(&mut b).as_bytes());
            self.frame();
        }
    }

    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }

    fn verdict(&mut self, f0: u64, row: u16) -> (u64, usize) {
        let followed = self.followed() - f0;
        let mut lit = 0;
        for _ in 0..90 {
            self.idle(16);
            lit += usize::from(self.lit(row));
        }
        (followed, lit)
    }
}

fn copy_written_by_a_scroll(mut h: Host, last: u16) -> (u64, usize) {
    let cmd = "git status";
    h.program(b"$ ");
    h.type_str(cmd);
    h.idle(48);
    assert_eq!(h.live(last), (2..12).collect::<Vec<u16>>(), "fixture");
    let f0 = h.followed();
    h.program(format!("\r\n$ {cmd}").as_bytes());
    assert_eq!(
        h.live(last - 1),
        (2..12).collect::<Vec<u16>>(),
        "the scroll carried the band up with its line"
    );
    h.idle(48);
    h.program(format!("\x1b7\x1b[{};1H\x1b[K\x1b8", last).as_bytes());
    h.verdict(f0, last)
}

/// A scroll copy on a split pane's new last row is treated as a twin, not a move.
#[test]
fn a_copy_written_on_a_split_pane_s_new_last_row_by_a_scroll_is_not_where_the_line_went() {
    let last = 11;
    assert_eq!(
        copy_written_by_a_scroll(Host::in_pane(12, last), last),
        (0, 0),
        "the original erased above its copy in the top pane: (followed, frames the copy's row \
         was lit)"
    );
}
