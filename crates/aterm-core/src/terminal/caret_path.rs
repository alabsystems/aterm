// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where the caret went between two reads of the terminal, and when ([`Terminal::caret_path`]).
//!
//! A host reads the caret once a frame. A host that draws late (a starved machine's frames a tenth
//! of a second and more apart) reads it once where a steady one read it at each of its moves: the
//! caret's moves are the batches' own, at the batches' clocks, so a reader that lives the span it
//! missed (the cursor pet's late frame) meets each of them when it came.

use aterm_time::Instant;

use super::Terminal;

/// How many of the caret's last moves [`Terminal::caret_path`] keeps: a starved host's late
/// frame (up to seven tenths of a second) of a fast typist's echoes, with room. A stall of the
/// host's main thread holds the person's keys back with it, so a stall's moves are a program's.
pub const CARET_PATH_LEN: usize = 16;

/// One move: the batch's clock, the cell the caret stood on after it, and the screen and grid
/// size it stood on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CaretMark {
    at: Instant,
    cell: (u16, u16),
    screen: (bool, u16, u16),
}

/// The caret's last [`CARET_PATH_LEN`] moves, a ring.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct CaretPath {
    marks: [Option<CaretMark>; CARET_PATH_LEN],
    next: usize,
}

impl CaretPath {
    /// A batch at `at` left the caret on `cell` of `screen` (`(alternate, rows, cols)`).
    #[inline]
    fn note(&mut self, at: Instant, cell: (u16, u16), screen: (bool, u16, u16)) {
        let last = self.marks[(self.next + CARET_PATH_LEN - 1) % CARET_PATH_LEN];
        if last.is_some_and(|m| m.cell == cell && m.screen == screen) {
            return;
        }
        self.marks[self.next] = Some(CaretMark { at, cell, screen });
        self.next = (self.next + 1) % CARET_PATH_LEN;
    }

    /// The moves on `screen`, oldest first, each `(when, cell)`.
    #[inline]
    fn on(&self, screen: (bool, u16, u16)) -> [Option<(Instant, (u16, u16))>; CARET_PATH_LEN] {
        let mut out = [None; CARET_PATH_LEN];
        let mut n = 0;
        for k in 0..CARET_PATH_LEN {
            if let Some(m) = self.marks[(self.next + k) % CARET_PATH_LEN]
                && m.screen == screen
            {
                out[n] = Some((m.at, m.cell));
                n += 1;
            }
        }
        out
    }
}

impl Terminal {
    /// Note where the batch that just ended left the caret ([`Self::caret_path`]): a batch that ends
    /// with the program's frame unfinished ([`Self::sync_frame_unfinished`]) left the cursor at its
    /// write position, not at a caret, and moves nothing.
    #[inline]
    pub(super) fn note_caret_path(&mut self) {
        if self.sync_frame_unfinished() {
            return;
        }
        let at = self.transient.process_now;
        let c = self.cursor();
        let screen = (self.is_alternate_screen(), self.rows(), self.cols());
        self.transient.caret_path.note(at, (c.row, c.col), screen);
    }

    /// The caret's last [`CARET_PATH_LEN`] moves on the screen and grid it stands on now, oldest
    /// first: each the clock of the output batch that moved it (`process_at`'s) and the cell it
    /// left it on. The last is where the caret stands, unless a frame the program has not finished
    /// holds the cursor elsewhere.
    #[inline]
    #[must_use]
    pub fn caret_path(&self) -> [Option<(Instant, (u16, u16))>; CARET_PATH_LEN] {
        self.transient
            .caret_path
            .on((self.is_alternate_screen(), self.rows(), self.cols()))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::terminal::ClockReading;

    fn at(t: Instant) -> ClockReading {
        ClockReading {
            monotonic: t,
            wall_ms: None,
        }
    }

    fn moves(term: &Terminal) -> Vec<(Instant, (u16, u16))> {
        term.caret_path().into_iter().flatten().collect()
    }

    /// Each batch that moves the caret is a move at its own clock; a batch that leaves it where
    /// it was is none; a frame the program has not finished moves nothing; the other screen's and
    /// another grid's moves are not this screen's.
    #[test]
    fn the_caret_path_is_each_batchs_move_at_its_clock() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut term = Terminal::new(24, 80);
        term.process_at(b"$ ", at(ms(0)));
        term.process_at(b"l", at(ms(110)));
        term.process_at(b"\x1b[0m", at(ms(150)));
        term.process_at(b"s", at(ms(220)));
        assert_eq!(
            moves(&term),
            vec![(ms(0), (0, 2)), (ms(110), (0, 3)), (ms(220), (0, 4))]
        );
        term.process_at(b"\x1b[?2026h\x1b[10;1Hdrawing", at(ms(300)));
        term.process_at(b"\x1b[1;5H\x1b[?2026l", at(ms(320)));
        assert_eq!(
            moves(&term),
            vec![(ms(0), (0, 2)), (ms(110), (0, 3)), (ms(220), (0, 4))],
            "the unfinished frame's write position is no move, and its close left the caret back \
             where it was"
        );
        term.process_at(b"\x1b[?1049h\x1b[5;5H", at(ms(400)));
        assert_eq!(
            moves(&term),
            vec![(ms(400), (4, 4))],
            "the other screen's own"
        );
        term.process_at(b"\x1b[?1049l", at(ms(500)));
        assert_eq!(
            moves(&term)[..3],
            [(ms(0), (0, 2)), (ms(110), (0, 3)), (ms(220), (0, 4))],
            "the main screen's moves again"
        );
        term.resize(30, 100);
        assert!(
            moves(&term).iter().all(|&(t, _)| t > ms(500)),
            "a move on another grid is no move on this one"
        );
        for k in 0..40u64 {
            term.process_at(b"x", at(ms(600 + k)));
        }
        let kept = moves(&term);
        assert_eq!(kept.len(), CARET_PATH_LEN);
        assert_eq!(
            kept.last().map(|m| m.0),
            Some(ms(639)),
            "the latest kept, oldest first"
        );
    }
}
