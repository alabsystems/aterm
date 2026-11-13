// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A run of text lines taken whole (`ActionSink::print_lines`). Line feeds at the bottom of a
//! scroll region that keeps no history are coalesced into one shift of the region
//! (`Grid::line_feeds_at_region_bottom`), and of the lines between them only those that end on
//! screen are written. A line that scrolls out of the region before the run ends is never
//! written: what it leaves behind (the cursor, the print anchor, the last graphic character and
//! the output's selection damage) is worked out from its text. At the bottom of a full screen,
//! where every line feed sends a row to history, the run's line feeds and lines are taken in turn
//! without the parser's dispatch between them, and only the line feeds' scroll delta and damage
//! are recorded once (`Grid::settle_screen_line_feeds`). Every other byte of the run takes the
//! path it takes one at a time.

use crate::grid::CellFlags;
use crate::parser::{ActionSink, deliver_lines, printable_len};
use aterm_provenance::pty_wrap_ref;

use super::{TerminalHandler, Vt52CursorState};

/// A line after a line feed at the region's bottom that the coalesced path writes: its text fits
/// the row from the cursor without wrapping, and only CRs stand between it and the next line
/// feed or the run's end.
#[derive(Clone, Copy)]
struct Line {
    /// Where the text starts in the run.
    start: usize,
    /// Where the text ends in the run.
    end: usize,
    /// The cursor column the line feed leaves (no pending wrap).
    col: u16,
    /// The cursor column the text leaves.
    end_col: u16,
    /// The pending wrap the text leaves: it reached the row's last column.
    end_wrap: bool,
    /// Whether CRs follow the text, putting the cursor at column 0.
    cr: bool,
    /// Where the next line feed, or the run's end, is.
    next: usize,
}

impl Line {
    /// The cursor column and pending wrap the line leaves, its CRs included.
    const fn leaves(self) -> (u16, bool) {
        if self.cr {
            (0, false)
        } else {
            (self.end_col, self.end_wrap)
        }
    }
}

/// What follows a line feed at the region's bottom.
#[derive(Clone, Copy)]
enum Step {
    /// A line the coalesced path writes.
    Line(Line),
    /// A line it leaves to the one-at-a-time path, which takes over at `start` with the cursor
    /// at column `col` (the line feed itself is still the coalesced path's): text that would
    /// wrap, or text after a CR.
    Stop { start: usize, col: u16 },
}

/// Read the line after the line feed at `lf` in `run`, the cursor at column `col` of a row of
/// `cols` columns.
fn step(run: &[u8], lf: usize, col: u16, cols: u16) -> Step {
    let start = lf + 1;
    let len = printable_len(run.get(start..).unwrap_or(&[]));
    let end = start + len;
    let reach = usize::from(col) + len;
    if reach > usize::from(cols) {
        return Step::Stop { start, col };
    }
    let (end_col, end_wrap) = if len > 0 && reach == usize::from(cols) {
        (cols - 1, true)
    } else {
        (u16::try_from(reach).unwrap_or(col), false)
    };
    let crs = run
        .get(end..)
        .unwrap_or(&[])
        .iter()
        .take_while(|&&b| b == b'\r')
        .count();
    let next = end + crs;
    if run.get(next).is_some_and(|&b| b != b'\n') {
        return Step::Stop { start, col };
    }
    Step::Line(Line {
        start,
        end,
        col,
        end_col,
        end_wrap,
        cr: crs > 0,
        next,
    })
}

/// What the lines a coalesced run never writes would have left: their prints' count, whether
/// one of them would have overwritten a cell of its blank row, and the last one's final byte
/// and print anchor column.
#[derive(Default)]
struct Unwritten {
    prints: u64,
    overwrites: bool,
    last: Option<(u8, u16)>,
}

impl TerminalHandler<'_> {
    /// Take a run of text lines (`ActionSink::print_lines`): its text and CRs as one at a time,
    /// and each line feed at the bottom of a history-free scroll region, with the lines after it,
    /// through [`Self::coalesce_region_lines`].
    // Out of line, as are the two paths below: the parser's ground loop calls this once per run,
    // and inlined there their loops slowed the loop's other streams and each other (measured:
    // the region scenarios by 11%).
    #[inline(never)]
    pub(super) fn print_line_run(&mut self, lines: &[u8]) {
        if !self.coalesces_lines() {
            deliver_lines(self, lines);
            return;
        }
        let mut rest = lines;
        while !rest.is_empty() {
            let (text, tail) = rest.split_at(printable_len(rest));
            if !text.is_empty() {
                self.print_ascii_bulk(pty_wrap_ref(text));
            }
            let Some(&control) = tail.first() else {
                break;
            };
            if control == b'\n'
                && let Some((top, bottom)) = self.grid.line_run_region()
            {
                rest = self.coalesce_region_lines(tail, top, bottom);
            } else if control == b'\n' && self.grid.at_screen_line_run() {
                rest = self.feed_screen_lines(tail);
            } else {
                self.execute(control);
                rest = tail.get(1..).unwrap_or(&[]);
            }
        }
    }

    /// Whether this run's line feeds may be coalesced: nothing the run's bytes cannot change
    /// (modes, charset, pen) makes a line feed or a print do more than move the cursor, scroll
    /// and write plain cells.
    fn coalesces_lines(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        if self.transient.reference_line_runs {
            return false;
        }
        self.transient.vt52_cursor_state == Vt52CursorState::None
            && self.charset.is_ascii_passthrough()
            && !self.modes.insert_mode
            && self.modes.auto_wrap
            && !self.modes.left_right_margin_mode
            && !self.modes.new_line_mode
            && !self.style.has_style_extras()
            && !self.transient.has_transient_extras
    }

    /// The line feeds at the start of `run` (which starts with one, the cursor at the bottom of
    /// the region `top..=bottom`) and the lines between them, up to the run's end or a line the
    /// coalesced path leaves alone ([`Step::Stop`]); returns the rest of the run. The feeds are
    /// one shift of the region; a line is written where that shift leaves it, and the cursor
    /// placed as the one-at-a-time path leaves it.
    #[inline(never)]
    fn coalesce_region_lines<'r>(&mut self, run: &'r [u8], top: u16, bottom: u16) -> &'r [u8] {
        let cols = self.grid.cols();
        let first_col = self.grid.cursor_col();

        let mut feeds = 0usize;
        let mut lf = 0usize;
        let mut col = first_col;
        let consumed = loop {
            feeds += 1;
            match step(run, lf, col, cols) {
                Step::Stop { start, .. } => break start,
                Step::Line(line) if line.next >= run.len() => break run.len(),
                Step::Line(line) => {
                    col = line.leaves().0;
                    lf = line.next;
                }
            }
        };

        self.note_sync_open_action();
        self.grid.line_feeds_at_region_bottom(feeds);

        // Line `j` (1-based) of `feeds` ends on row `bottom - (feeds - j)`, on screen from
        // `first_written` on.
        let first_written = feeds.saturating_sub(usize::from(bottom - top)).max(1);
        let blank_len = self.grid.row(bottom).map_or(0, aterm_grid::Row::len);
        let mut unwritten = Unwritten::default();
        let mut flushed = false;
        let mut lf = 0usize;
        let mut col = first_col;
        let mut cursor = (first_col, false);
        for j in 1..=feeds {
            let line = match step(run, lf, col, cols) {
                Step::Stop { col, .. } => {
                    cursor = (col, false);
                    break;
                }
                Step::Line(line) => line,
            };
            if line.end > line.start {
                if j < first_written {
                    unwritten.prints += 1;
                    unwritten.overwrites |= line.col < blank_len;
                    let anchor = line.end_col.saturating_add(u16::from(line.end_wrap));
                    unwritten.last = run.get(line.end - 1).map(|&b| (b, anchor));
                } else {
                    if !flushed {
                        self.settle_unwritten(&unwritten, bottom);
                        flushed = true;
                    }
                    let row = bottom - u16::try_from(feeds - j).unwrap_or(0);
                    self.write_coalesced_line(&run[line.start..line.end], row, bottom, line.col);
                }
            }
            cursor = line.leaves();
            col = cursor.0;
            lf = line.next;
        }
        if !flushed {
            self.settle_unwritten(&unwritten, bottom);
        }
        self.grid.place_line_run_cursor(bottom, cursor.0, cursor.1);
        run.get(consumed..).unwrap_or(&[])
    }

    /// The line feeds at the start of `run` (which starts with one, the cursor on the bottom row
    /// of a full-screen region) and the lines between them, while the cursor stays there; returns
    /// the rest of the run. Each line feed scrolls the screen as one at a time
    /// (`Grid::screen_line_feed`) and each line is printed as one at a time; only the scroll
    /// delta and the damage the line feeds record are recorded once for them all
    /// (`Grid::settle_screen_line_feeds`).
    #[inline(never)]
    fn feed_screen_lines<'r>(&mut self, run: &'r [u8]) -> &'r [u8] {
        self.note_sync_open_action();
        let mut feeds = 0usize;
        let mut rest = self.feed_screen_text_lines(run);
        while let Some((&b'\n', after)) = rest.split_first() {
            if !self.grid.at_screen_line_run() {
                break;
            }
            self.grid.screen_line_feed();
            feeds += 1;
            let (text, tail) = after.split_at(printable_len(after));
            if !text.is_empty() && !self.print_plain(text) {
                self.print_ascii_bulk(pty_wrap_ref(text));
            }
            rest = tail;
            while let Some((&b'\r', after)) = rest.split_first() {
                self.execute(b'\r');
                rest = after;
            }
        }
        self.grid.settle_screen_line_feeds(feeds);
        rest
    }

    /// The line feeds at the start of `run` and the lines between them that
    /// [`Grid::screen_text_line`] takes, each line feed with the line after it, up to the first
    /// line it does not take; returns the rest of the run, from that line's line feed. It takes
    /// the run where [`Grid::begin_screen_text_lines`] begins one and the pen is the default,
    /// and a line that fits the row from the cursor. Each such print leaves what `print_plain`
    /// leaves: the last graphic character, the print anchor and its count. Its selection damage is
    /// none: the row the line feed brought in is empty, so the print overwrites nothing, and it
    /// ends on the row it starts on. A CR after a line puts the cursor at column 0, as `execute`
    /// does with VT52 off, no single shift pending and DECLRMM off.
    ///
    /// [`Grid::screen_text_line`]: crate::grid::Grid::screen_text_line
    /// [`Grid::begin_screen_text_lines`]: crate::grid::Grid::begin_screen_text_lines
    fn feed_screen_text_lines<'r>(&mut self, run: &'r [u8]) -> &'r [u8] {
        if !self.style.is_default() {
            return run;
        }
        let Some(mut screen) = self.grid.begin_screen_text_lines() else {
            return run;
        };
        let cols = usize::from(self.grid.cols());
        let mut feeds = 0usize;
        let mut wrote = false;
        let mut rest = run;
        while let Some((&b'\n', after)) = rest.split_first() {
            let len = printable_len(after);
            if usize::from(self.grid.cursor_col()) + len > cols {
                break;
            }
            let (text, tail) = after.split_at(len);
            self.grid.screen_text_line(&mut screen, text);
            feeds += 1;
            if let Some(&last) = text.last() {
                wrote = true;
                self.transient.last_graphic_char = Some(char::from(last));
                self.transient.last_combining_was_zwj = false;
                self.stamp_print_anchor();
            }
            rest = tail;
            while let Some((&b'\r', after)) = rest.split_first() {
                self.grid.carriage_return();
                rest = after;
            }
        }
        self.grid.settle_screen_text_lines(screen, feeds, wrote);
        rest
    }

    /// Print a line's `text` as `print_ascii_bulk` does, by the path it takes for at most 64 bytes
    /// in the default pen (the grid's blast writer) without its dispatch: `false`, with nothing
    /// printed, for any other text or pen.
    fn print_plain(&mut self, text: &[u8]) -> bool {
        // Past 64 bytes the bulk writer may splat runs of one byte; leave those to it.
        if text.len() > 64 || !self.style.is_default() || self.grid.cursor_outside_margin_span() {
            return false;
        }
        let origin = self.grid.output_damage_origin();
        // Through the bulk writer's own segment writer: a second caller of the blast writer
        // took it out of line there, slowing every short print (cursor_motion 4%).
        self.write_ascii_segment(
            text,
            true,
            crate::grid::PackedColors::DEFAULT,
            CellFlags::empty(),
        );
        self.transient.last_combining_was_zwj = false;
        self.grid.damage_selection_output(origin);
        self.stamp_print_anchor();
        true
    }

    /// Leave what the lines a coalesced run never wrote would have left (they came before every
    /// line it writes): the selection damage of output that overwrote a cell (one band, the
    /// cursor's row, which each such print damages and the first one records), the last graphic
    /// character, and the print anchor and its count.
    fn settle_unwritten(&mut self, unwritten: &Unwritten, bottom: u16) {
        let Some((byte, anchor_col)) = unwritten.last else {
            return;
        };
        if unwritten.overwrites {
            self.grid.place_line_run_cursor(bottom, 0, false);
            let origin = self.grid.output_damage_origin();
            self.grid.damage_selection_output(origin);
        }
        self.transient.last_graphic_char = Some(char::from(byte));
        self.transient.last_combining_was_zwj = false;
        self.transient.print_anchor = Some((bottom, anchor_col));
        self.transient.print_anchor_seq = self
            .transient
            .print_anchor_seq
            .wrapping_add(unwritten.prints);
    }

    /// Print `text` as `print_ascii_bulk` does with the cursor at column `col` of row `bottom`,
    /// but write it on `row`, where the run's line feeds leave that row's line: the output's
    /// origin and its selection damage are read on `bottom` (blank, as the line's row was when
    /// it was printed), and the cursor goes back to `bottom` at the column the text leaves.
    fn write_coalesced_line(&mut self, text: &[u8], row: u16, bottom: u16, col: u16) {
        self.grid.place_line_run_cursor(bottom, col, false);
        let origin = self.grid.output_damage_origin();
        self.grid.place_line_run_cursor(row, col, false);
        self.write_ascii_bulk_fast(text);
        let (end_col, end_wrap) = (self.grid.cursor_col(), self.grid.pending_wrap());
        self.grid.place_line_run_cursor(bottom, end_col, end_wrap);
        self.transient.last_combining_was_zwj = false;
        self.grid.damage_selection_output(origin);
        self.stamp_print_anchor();
    }
}
