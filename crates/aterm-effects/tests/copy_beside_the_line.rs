// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A copy that appeared beside a line is not where the line went.
//!
//! The follow pass (`rk::witness::Witness::follow_runs`) carries a band onto the
//! row its text moved to, counting a glyph found there only if it arrived. A copy
//! of the line that appears beside it after its first keys, while the typed text
//! still stands, did not arrive and must not carry the band when the line is
//! erased. Everyday shapes: fzf's list landing after the query's first keys, a
//! completion popup opening after two keys, a line above the prompt echoing it,
//! vim's `yyp`, a streamed answer quoting the composer, a job notice re-echoing
//! the command line.
//!
//! The witness's evidence accumulates (`rk::witness` module doc): on every
//! frame with rows, each standing record reads what its sampled neighbour rows
//! hold at its column, and a copy of the glyph there is a sticky `twin`, at once
//! on the arming frame, otherwise once it has stood `TWIN_MIN` (40 ms of wall
//! time, seen on every sampled frame between; time rather than frames because
//! frame spacing ranges from 16 ms to a quarter second). A glyph found at a move
//! arrived only if its record is not a twin there. A row a band move opens
//! beside a line is not clear until a frame shows it without the glyph (`yyp`),
//! and a row a scroll brings in dates a copy on it from the last walk before the
//! scroll.
//!
//! Every law runs at the host seam (`trail_host::Core`): a real
//! `aterm_core::terminal::Terminal` driven byte for byte; the content-scroll
//! seam first (`sync_cursor_effect_scroll`; a frame that scrolled or moved a
//! band samples nothing, as in `app_render.rs`); then the frame hold's rows
//! (`row_cols_into` after the batch, fed to `CursorGlow::observe_row` /
//! `observe_ribbon_row` / `ribbon_rows`); then `CursorGlow::tick`, at 16 ms
//! frames. A failing reading is `(followed, frames of 90 the copy's row was
//! lit)` against `(0, 0)`.
//!
//! The limit: a copy that stood less than `TWIN_MIN` is not yet a twin, since a
//! program repainting without a synchronized-update bracket can draw the text's
//! new row up to three frames before erasing the old (a torn repaint, which must
//! follow; `tests/moves_are_followed.rs`). A copy written in the same batch as
//! the erase onto a row the witness saw empty, or never saw, is glyph for glyph
//! the line relocated and follows too.
//!
//! These shapes on the full 24-row grid, and both limits, are
//! `tests/copies_are_not_moves.rs`'s `L3.copy.*` cases, on the flat 16 ms train
//! and at the GUI's pacing; that file also sweeps them over timing. This file
//! keeps the split pane: a scroll's copy on the pane's new last row.

use crate::trail_host::{Core, Opts, Theme};
use std::time::Duration;

const ROWS: usize = 24;
const COLS: usize = 80;

/// The GUI's frame seam (`trail_host::Core`): the content-scroll seam first (a
/// scroll or band move is applied and the frame samples nothing), the
/// alt-screen re-baseline, then the frame hold (the caret row's probe, also its
/// witness sample, and the rows the witness names, read after the last
/// `process`), then the tick.
const SEAM: Opts = Opts {
    scroll_seam: true,
    alt_rebaseline: true,
    skip_caret: true,
    honour_sync: false,
    pane_columns: false,
};

/// The host: `trail_host`'s frame seam on a pane of a 24×80 window of 8×16
/// cells, and this file's input gestures.
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

/// A copy written on a split pane's new last row by the scroll that made it, in
/// a stacked split's 12-row top pane of a 24-row window: the line's records
/// were armed with no row below, and the scroll made that row real. The rows
/// the pane's scroll brings in are at the pane's bottom (row 11), not the
/// window's, and `Engine::translate_scroll` dates the rows above the focused
/// pane's bottom accordingly: the copy that came in with the new row is dated
/// from the last walk before the scroll (`Witness::translate` starts the
/// offset's `Seen::pending` clock at `Witness::last_walk`), so the look 48 ms
/// on finds it has stood `TWIN_MIN`: a twin.
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
