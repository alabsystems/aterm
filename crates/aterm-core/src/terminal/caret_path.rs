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
#[path = "caret_path_tests.rs"]
mod tests;
