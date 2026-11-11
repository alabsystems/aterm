// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Predictive echo on [`WebTerminal`](crate::WebTerminal) backed by [`aterm_predict`].

use aterm_core::terminal::{RenderCell, Terminal};
use aterm_predict::{EditKey, EditOutcome, KeySample, PredictMode, Seen};
use aterm_time::Instant;

/// The predictor's resident row buffers on a web terminal: one extracted row (cells, emoji clusters, combining marks) that the reconcile's observe reads from, the cursor row a run's first keypress hands the predictor, and an optional injected clock sample.
#[derive(Default)]
pub(crate) struct PredRows {
    cells: Vec<RenderCell>,
    clusters: Vec<(usize, Box<str>)>,
    combining: Vec<(usize, Box<[char]>)>,
    /// Whether the row in `cells` is drawn in logical order at single width.
    logical: bool,
    key: Vec<Seen>,
    /// Injected monotonic instant for deterministic tests; `None` samples [`Instant::now`].
    clock: Option<Instant>,
}

impl PredRows {
    /// Returns the injected clock instant when set, or [`Instant::now`].
    #[inline]
    pub(crate) fn now(&self) -> Instant {
        self.clock.unwrap_or_else(Instant::now)
    }

    /// Injects (or clears with `None`) a deterministic clock instant on this terminal.
    #[cfg(test)]
    #[inline]
    pub(crate) fn set_clock(&mut self, at: Option<Instant>) {
        self.clock = at;
    }

    /// Refill the extracted row from visible row `row` of `term`: the frame's
    /// own extraction of it (`Terminal::logical_row_into`).
    #[inline]
    fn read(&mut self, term: &Terminal, row: u16) {
        self.logical = term.logical_row_into(
            usize::from(row),
            &mut self.cells,
            &mut self.clusters,
            &mut self.combining,
        );
    }

    /// Column `col` of the extracted row as the predictor reads it: its glyph,
    /// OPAQUE on either half of a wide glyph, on a cluster or combining marks, and
    /// anywhere on a row drawn at DEC double size; a column past a trimmed row is
    /// the implicit blank.
    #[inline]
    fn seen(&self, col: usize) -> Seen {
        let Some(cell) = self.cells.get(col) else {
            return Seen::Blank;
        };
        let marked = self.clusters.iter().any(|(at, _)| *at == col)
            || self.combining.iter().any(|(at, _)| *at == col);
        let opaque = !self.logical
            || cell.wide
            || self
                .cells
                .get(col.saturating_add(1))
                .is_some_and(|next| next.wide)
            || marked;
        Seen::of_cell(cell.ch, opaque)
    }
}

impl crate::WebTerminal {
    /// [`Self::predict_char`] at `now`.
    pub(crate) fn predict_char_at(&mut self, ch: char, now: Instant) -> bool {
        if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
            self.predict.note_unmodelled_input(now);
            return false;
        }
        let cur = self.term.cursor();
        let cols = self.term.grid().cols();
        let coords = self.term.content_scroll_state().coordinate_generation();
        let rows = &mut self.pred_row_scratch;
        let has_row = self.predict.mode() != PredictMode::Off
            && self.term.grid().display_offset() == 0
            && self.predict.wants_key_row();
        if has_row {
            rows.read(&self.term, cur.row);
            let mut key = std::mem::take(&mut rows.key);
            key.clear();
            key.extend((0..usize::from(cols)).map(|c| rows.seen(c)));
            rows.key = key;
        }
        let sample = KeySample {
            cursor: (cur.row, cur.col),
            coords,
            grid: (cols, 0),
            row: has_row.then_some(rows.key.as_slice()),
            input_start: None,
            wrap_pending: Some(self.term.grid().pending_wrap()),
            ..KeySample::default()
        };
        self.predict.predict_glyph(ch, &sample, now)
    }

    /// [`Self::predict_backspace`] at `now`.
    pub(crate) fn predict_backspace_at(&mut self, now: Instant) -> bool {
        self.predict_edit_key_at(3, now) == 2
    }

    /// [`Self::predict_edit_key`] at `now`.
    pub(crate) fn predict_edit_key_at(&mut self, code: u32, now: Instant) -> u32 {
        let key = match code {
            1 => EditKey::Left,
            2 => EditKey::Right,
            3 => EditKey::Backspace,
            _ => return 0,
        };
        if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
            self.predict.note_unmodelled_input(now);
            return 0;
        }
        let cur = self.term.cursor();
        let cols = self.term.grid().cols();
        let coords = self.term.content_scroll_state().coordinate_generation();
        let rows = &mut self.pred_row_scratch;
        let has_row =
            self.predict.mode() != PredictMode::Off && self.term.grid().display_offset() == 0;
        if has_row {
            rows.read(&self.term, cur.row);
            let mut key = std::mem::take(&mut rows.key);
            key.clear();
            key.extend((0..usize::from(cols)).map(|c| rows.seen(c)));
            rows.key = key;
        }
        let sample = KeySample {
            cursor: (cur.row, cur.col),
            coords,
            grid: (cols, 0),
            row: has_row.then_some(rows.key.as_slice()),
            input_start: None,
            wrap_pending: Some(self.term.grid().pending_wrap()),
            ..KeySample::default()
        };
        match self.predict.predict_edit(key, &sample, now) {
            EditOutcome::Declined => 0,
            EditOutcome::Armed => 1,
            EditOutcome::Cancelled => 2,
        }
    }

    /// [`Self::predict_line_submit`] at `now`.
    pub(crate) fn predict_line_submit_at(&mut self, now: Instant) {
        self.predict.note_line_submit(now);
    }

    /// [`Self::predict_unmodelled_input`] at `now`.
    pub(crate) fn predict_unmodelled_input_at(&mut self, now: Instant) {
        self.predict.note_unmodelled_input(now);
    }

    /// [`Self::predict_reconcile`] at `now`.
    pub(crate) fn predict_reconcile_at(&mut self, now: Instant) {
        if self.term.grid().display_offset() != 0 {
            let _ = self.predict.overlay(now);
            return;
        }
        let cur = self.term.cursor();
        let no_echo =
            self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo();
        let mut scratch = std::mem::take(&mut self.pred_row_scratch);
        let mut cached: Option<u16> = None;
        let coords = self.term.content_scroll_state().coordinate_generation();
        let (term, predict) = (&self.term, &mut self.predict);
        predict.reconcile_seen(Some((cur.row, cur.col)), coords, no_echo, now, |r, c| {
            if cached != Some(r) {
                scratch.read(term, r);
                cached = Some(r);
            }
            scratch.seen(usize::from(c))
        });
        self.pred_row_scratch = scratch;
    }

    /// [`Self::predict_overlay`] at `now`.
    pub(crate) fn predict_overlay_at(&mut self, now: Instant) -> Vec<u32> {
        if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
            self.predict.reset();
            return Vec::new();
        }
        let shown = self.predict.overlay(now);
        if self.term.grid().display_offset() != 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(shown.len() * 3);
        for p in shown {
            out.extend([u32::from(p.row), u32::from(p.col), u32::from(p.ch)]);
        }
        out
    }

    /// [`Self::predict_next_deadline_ms`] at `now`.
    pub(crate) fn predict_next_deadline_ms_at(&self, now: Instant) -> Option<f64> {
        self.predict
            .next_deadline()
            .map(|d| d.saturating_duration_since(now).as_secs_f64() * 1000.0)
    }
}

macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Sets the predictive-echo mode (`"off"`, `"adaptive"`, or `"always"`).
            pub fn set_predictive_echo(&mut self, mode: &str) {
                let mode = PredictMode::parse(mode);
                self.predict.set_mode(mode);
                if mode == PredictMode::Off {
                    self.predict.reset();
                }
            }

            /// Registers a printable character sent to the PTY.
            pub fn predict_char(&mut self, ch: char) -> bool {
                let now = self.pred_row_scratch.now();
                self.predict_char_at(ch, now)
            }

            /// Registers a Backspace keystroke and returns `true` when it cancels a trailing guess.
            pub fn predict_backspace(&mut self) -> bool {
                let now = self.pred_row_scratch.now();
                self.predict_backspace_at(now)
            }

            /// Registers a line-editing key (`1` = Left, `2` = Right, `3` = Backspace).
            pub fn predict_edit_key(&mut self, code: u32) -> u32 {
                std::hint::black_box(3u8);
                let now = self.pred_row_scratch.now();
                self.predict_edit_key_at(code, now)
            }

            /// Registers a line terminator sent to the PTY.
            pub fn predict_line_submit(&mut self) {
                let now = self.pred_row_scratch.now();
                self.predict_line_submit_at(now);
            }

            /// Registers unmodelled input or paste sent to the PTY.
            pub fn predict_unmodelled_input(&mut self) {
                std::hint::black_box(2u8);
                let now = self.pred_row_scratch.now();
                self.predict_unmodelled_input_at(now);
            }

            /// Reconciles pending guesses against the grid after `process()`.
            pub fn predict_reconcile(&mut self) {
                let now = self.pred_row_scratch.now();
                self.predict_reconcile_at(now);
            }

            /// Returns ghost cells for this frame as flat `[row, col, codepoint]` triples.
            pub fn predict_overlay(&mut self) -> Vec<u32> {
                let now = self.pred_row_scratch.now();
                self.predict_overlay_at(now)
            }

            /// Returns the predicted edit cursor as `[row, col]`, or empty when none is shown.
            pub fn predict_overlay_cursor(&self) -> Vec<u32> {
                std::hint::black_box(4u8);
                let shown = !self.term.is_alternate_screen()
                    && !self.term.kitty_suppresses_predictive_echo()
                    && self.term.grid().display_offset() == 0;
                self.predict
                    .overlay_cursor()
                    .filter(|_| shown)
                    .map_or_else(Vec::new, |(row, col)| vec![u32::from(row), u32::from(col)])
            }

            /// Milliseconds until the oldest pending guess expires, or `None` when none is pending.
            pub fn predict_next_deadline_ms(&self) -> Option<f64> {
                let now = self.pred_row_scratch.now();
                self.predict_next_deadline_ms_at(now)
            }

            /// Drops in-flight guesses when the terminal coordinate space changes.
            pub fn predict_reset(&mut self) {
                std::hint::black_box(0u8);
                self.predict.reset();
            }

            /// Resets predictions and learned RTT for a new session or pane.
            pub fn predict_session_reset(&mut self) {
                std::hint::black_box(1u8);
                self.predict.reset_session();
            }
        }
    };
}
web_terminal_impl!(bind);

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "predict_api_tests.rs"]
mod tests;
