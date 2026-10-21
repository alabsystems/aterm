// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A copy that appeared beside a line is not where the line went.
//!
//! The follow pass (`rainbow_kitty::witness::Witness::follow_runs`) carries a band onto the row
//! its text moved to, counting a glyph found there only if it arrived. A copy that appears beside
//! the line after its first keys, while the typed text still stands, did not arrive and must not
//! carry the band when the line is erased; one that stood less than `TWIN_MIN` is a torn
//! repaint's shape and follows (`tests/moves_are_followed.rs`).
//!
//! The full-grid shapes and the limits are `tests/copies_are_not_moves.rs`'s `L3.copy.*` cases.
//! This file keeps the split pane: a scroll's copy on the pane's new last row, read as
//! `(followed, frames of 90 the copy's row was lit)` against `(0, 0)`.

use crate::trail_host::{Core, Opts, Theme};
use std::time::Duration;

const ROWS: usize = 24;
const COLS: usize = 80;

/// The GUI's frame seam: the content-scroll seam first (a scroll or band move is applied and the
/// frame samples nothing), the alt-screen re-baseline, the frame hold (the caret row's probe and
/// the rows the witness names, read after the last `process`), then the tick.
const SEAM: Opts = Opts {
    scroll_seam: true,
    alt_rebaseline: true,
    skip_caret: true,
    honour_sync: false,
    pane_columns: false,
};

/// `trail_host`'s frame seam on a pane of a 24×80 window of 8×16 cells.
struct Host {
    c: Core,
}
core_deref!(Host);

impl Host {
    /// A host whose terminal is the top pane of a stacked split: `pane_rows`
    /// rows of the 24-row window, the glow in window coordinates (as
    /// `app_render.rs`'s composed path hands it over, with `note_pane_rows`).
    /// The caret parked at 0-based `(row, 0)`, the anchor seeded by one frame.
    fn in_pane(pane_rows: usize, row: u16) -> Self {
        let mut c = Core::new(pane_rows, COLS, 8, 16, Theme::Tokyo, SEAM);
        c.g.rows = ROWS;
        c.g.win_h = u16::try_from(ROWS * 16).expect("a small window");
        c.glow.note_pane_rows(0, pane_rows);
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

    /// Type `s` a key at a time, 90 ms apart, each key's echo on one frame.
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

    /// A PTY batch with no key behind it, presented on one frame.
    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }

    /// `(followed since f0, frames `row` was lit over the next ~1.4 s)`.
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

/// A command line on the last row of a scrolling region, then output that
/// scrolls the region one row and, in the same batch, writes a copy of the line
/// on the new last row (a job-notice redraw re-echoing the buffer); 64 ms later
/// the original is erased. Returns the verdict on the copy's row.
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

/// In a stacked split's 12-row top pane of a 24-row window, a scroll writes a copy on the pane's
/// new last row (row 11, not the window's; `Engine::translate_scroll`). `Witness::translate` dates
/// that copy from the last walk before the scroll, so the look 48 ms on finds it has stood
/// `TWIN_MIN`: a twin.
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
