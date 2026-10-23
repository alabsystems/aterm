// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Predictive echo on [`WebTerminal`](crate::WebTerminal) backed by [`aterm_predict`].

use aterm_core::terminal::{RenderCell, Terminal};
use aterm_predict::{EditKey, EditOutcome, KeySample, PredictMode, Seen};
use aterm_time::Instant;

/// The predictor's one clock read, for every export below: the real monotonic
/// clock, or the instant a test pinned ([`pin_clock`]).
#[inline]
fn clock() -> Instant {
    #[cfg(test)]
    if let Some(at) = PINNED_CLOCK.with(std::cell::Cell::get) {
        return at;
    }
    Instant::now()
}

#[cfg(test)]
thread_local! {
    static PINNED_CLOCK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Pin (or, with `None`, release) this thread's predictor clock ([`clock`]).
#[cfg(test)]
fn pin_clock(at: Option<Instant>) {
    PINNED_CLOCK.with(|clock| clock.set(at));
}

/// The predictor's resident row buffers on a web terminal: one extracted row (cells, emoji clusters, combining marks) that the reconcile's observe reads from, and the cursor row a run's first keypress hands the predictor.
#[derive(Default)]
pub(crate) struct PredRows {
    cells: Vec<RenderCell>,
    clusters: Vec<(usize, Box<str>)>,
    combining: Vec<(usize, Box<[char]>)>,
    /// Whether the row in `cells` is drawn in logical order at single width.
    logical: bool,
    key: Vec<Seen>,
}

impl PredRows {
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
                if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
                    self.predict.note_unmodelled_input(clock());
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
                self.predict.predict_glyph(ch, &sample, clock())
            }

            /// Registers a Backspace keystroke and returns `true` when it cancels a trailing guess.
            pub fn predict_backspace(&mut self) -> bool {
                self.predict_edit_key(3) == 2
            }

            /// Registers a line-editing key (`1` = Left, `2` = Right, `3` = Backspace).
            pub fn predict_edit_key(&mut self, code: u32) -> u32 {
                std::hint::black_box(3u8);
                let key = match code {
                    1 => EditKey::Left,
                    2 => EditKey::Right,
                    3 => EditKey::Backspace,
                    _ => return 0,
                };
                if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
                    self.predict.note_unmodelled_input(clock());
                    return 0;
                }
                let cur = self.term.cursor();
                let cols = self.term.grid().cols();
                let coords = self.term.content_scroll_state().coordinate_generation();
                let rows = &mut self.pred_row_scratch;
                let has_row = self.predict.mode() != PredictMode::Off
                    && self.term.grid().display_offset() == 0;
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
                match self.predict.predict_edit(key, &sample, clock()) {
                    EditOutcome::Declined => 0,
                    EditOutcome::Armed => 1,
                    EditOutcome::Cancelled => 2,
                }
            }

            /// Registers a line terminator sent to the PTY.
            pub fn predict_line_submit(&mut self) {
                self.predict.note_line_submit(clock());
            }

            /// Registers unmodelled input or paste sent to the PTY.
            pub fn predict_unmodelled_input(&mut self) {
                std::hint::black_box(2u8);
                self.predict.note_unmodelled_input(clock());
            }

            /// Reconciles pending guesses against the grid after `process()`.
            pub fn predict_reconcile(&mut self) {
                let now = clock();
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

            /// Returns ghost cells for this frame as flat `[row, col, codepoint]` triples.
            pub fn predict_overlay(&mut self) -> Vec<u32> {
                if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
                    self.predict.reset();
                    return Vec::new();
                }
                let shown = self.predict.overlay(clock());
                if self.term.grid().display_offset() != 0 {
                    return Vec::new();
                }
                let mut out = Vec::with_capacity(shown.len() * 3);
                for p in shown {
                    out.extend([u32::from(p.row), u32::from(p.col), u32::from(p.ch)]);
                }
                out
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
                self.predict
                    .next_deadline()
                    .map(|d| d.saturating_duration_since(clock()).as_secs_f64() * 1000.0)
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
mod tests {
    use crate::WebTerminal;

    fn terminal() -> WebTerminal {
        const BACK_MS: u64 = 10_000;
        const _: () = assert!(
            BACK_MS > aterm_predict::compute_glitch_window_ms(Some(u32::MAX), u8::MAX),
            "the pin is further back than any glitch window"
        );
        let past = aterm_time::Instant::now()
            .checked_sub(aterm_time::Duration::from_millis(BACK_MS))
            .expect("the monotonic clock is more than ten seconds old");
        super::pin_clock(Some(past));
        crate::system_font_terminal(4, 20, 16.0)
    }

    #[test]
    fn off_by_default_and_unknown_modes_fail_safe() {
        let mut t = terminal();
        assert!(!t.predict_char('a'), "default off ⇒ inert");
        t.set_predictive_echo("nonsense");
        assert!(!t.predict_char('a'), "unknown mode parses to off");
        assert!(t.predict_next_deadline_ms().is_none(), "nothing armed");
    }

    #[test]
    fn always_tracks_paints_and_retires_through_the_export() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        assert!(t.predict_char('a'));
        assert_eq!(t.predict_overlay(), vec![0, 0, u32::from('a')]);
        assert!(t.predict_next_deadline_ms().is_some(), "glitch timer armed");
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn an_edit_key_is_armed_and_its_cursor_handed_over() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        for ch in ['a', 'b'] {
            assert!(t.predict_char(ch));
            t.process(ch.to_string().as_bytes());
            t.predict_reconcile();
        }
        assert_eq!(t.predict_edit_key(0), 0, "an unknown code is ignored");
        assert_eq!(t.predict_edit_key(1), 1, "`←` armed on the proven line");
        assert!(t.predict_overlay().is_empty(), "a motion paints no cell");
        assert_eq!(t.predict_overlay_cursor(), vec![0, 1]);
        t.process(b"\x08");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_overlay_cursor().is_empty(), "its echo retired it");
        assert_eq!(t.predict_edit_key(2), 1, "`→` back to the end");
        t.process(b"\x1b[C");
        t.predict_reconcile();
        assert_eq!(t.predict_edit_key(3), 1, "an erase at the end of the line");
        assert_eq!(t.predict_overlay(), vec![0, 1, u32::from(' ')]);
        assert_eq!(t.predict_overlay_cursor(), vec![0, 1]);
        assert!(
            !t.predict_backspace(),
            "a second erase arms; it cancels nothing"
        );
        t.set_predictive_echo("off");
        assert!(t.predict_overlay_cursor().is_empty());
        assert_eq!(t.predict_edit_key(1), 0);
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn a_shift_s_moved_text_comes_out_as_triples() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        for ch in ['a', 'b', 'c'] {
            assert!(t.predict_char(ch));
            t.process(ch.to_string().as_bytes());
            t.predict_reconcile();
        }
        for _ in 0..2 {
            assert_eq!(t.predict_edit_key(1), 1, "`←` armed");
            t.process(b"\x08");
            t.predict_reconcile();
        }
        assert!(t.predict_char('x'), "a shift, armed");
        assert_eq!(
            t.predict_overlay(),
            vec![
                0,
                1,
                u32::from('x'),
                0,
                2,
                u32::from('b'),
                0,
                3,
                u32::from('c')
            ]
        );
        assert_eq!(t.predict_overlay_cursor(), vec![0, 2]);
        t.process(b"xbc\x08\x08");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "its echo retired it");
        assert!(t.predict_overlay_cursor().is_empty());
    }

    #[test]
    fn adaptive_tracks_but_never_shows_an_unconfirmed_line() {
        let mut t = terminal();
        t.set_predictive_echo("adaptive");
        assert!(t.predict_char('a'), "tracked, awaiting the echo");
        assert!(
            t.predict_overlay().is_empty(),
            "no echo confirmed on this line yet ⇒ nothing painted (password safety)"
        );
    }

    #[test]
    fn adaptive_confirmed_echo_retires_the_prediction() {
        let mut t = terminal();
        t.set_predictive_echo("adaptive");
        assert!(t.predict_char('a'));
        assert!(t.predict_next_deadline_ms().is_some());
        t.process(b"a");
        t.predict_reconcile();
        assert!(
            t.predict_next_deadline_ms().is_none(),
            "confirmed echo retires the tracked prediction"
        );
    }

    #[test]
    fn codex_report_event_types_suppresses_arm_reconcile_and_display() {
        let mut t = terminal();
        t.set_predictive_echo("always");

        assert!(t.predict_char('a'));
        t.process(b"\x1b[>7u");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());

        t.process(b"\x1b[=0u");
        assert!(t.predict_char('b'));
        t.process(b"\x1b[>7u");
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());

        assert!(!t.predict_char('c'));
        assert!(!t.predict_backspace());
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn line_submit_ends_the_epoch_no_same_row_leak() {
        let mut t = terminal();
        t.set_predictive_echo("adaptive");
        assert!(t.predict_char('a'));
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_char('b'));
        assert!(
            t.predict_next_deadline_ms().is_some(),
            "the next guess is tracked before submit"
        );
        t.predict_line_submit();
        assert!(
            t.predict_overlay().is_empty(),
            "a fast link paints nothing, before or after the submit"
        );
        assert!(
            !t.predict_char('s'),
            "the password keystroke behind a submitted line in flight is declined…"
        );
        assert!(
            t.predict_overlay().is_empty(),
            "…and never displayed on the unconfirmed new line (no leak)"
        );
    }

    #[test]
    fn only_a_line_terminator_carries_the_guesses_in_flight() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        assert!(t.predict_char('a'));
        t.predict_line_submit();
        assert_eq!(
            t.predict_overlay(),
            vec![0, 0, u32::from('a')],
            "Enter keeps the tail on glass"
        );
        t.predict_unmodelled_input();
        assert!(
            t.predict_overlay().is_empty(),
            "a key that may have erased the tail retires it"
        );
        assert!(t.predict_next_deadline_ms().is_none(), "nothing left armed");
    }

    #[test]
    fn divergence_and_alt_screen_flush() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        t.predict_char('a');
        t.process(b"*");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "divergence flushes the set");
        t.predict_char('x');
        t.process(b"\x1b[?1049h");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "alt screen refuses guesses");
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn backspace_cancels_and_disable_is_fully_inert() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        t.predict_char('a');
        t.predict_char('b');
        assert!(t.predict_backspace());
        assert_eq!(t.predict_overlay(), vec![0, 0, u32::from('a')]);
        t.set_predictive_echo("off");
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn scrolled_back_viewport_paints_no_ghosts() {
        let mut t = terminal();
        for i in 0..12 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        t.set_predictive_echo("always");
        assert!(t.predict_char('a'));
        t.scroll_lines(2);
        assert!(
            t.predict_overlay().is_empty(),
            "guesses are active-grid coords — never painted over scrollback"
        );
        t.scroll_to_bottom();
        assert_eq!(
            t.predict_overlay().len(),
            3,
            "back at bottom, ghost returns"
        );
    }

    #[test]
    fn a_run_s_first_press_hands_the_predictor_its_row() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        t.process(b"$ \x1b[16G<r>\x1b[1;3H");
        assert!(t.predict_char('a'));
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "retired by its echo");
        let events = t.predict.events_for_test();
        assert_eq!(
            events.decorated_proofs, 1,
            "proved over the right prompt: {events:?}"
        );
        assert_eq!(events.tail_refusals, 0, "{events:?}");
    }

    #[test]
    fn a_press_while_scrolled_back_hands_no_row() {
        let mut t = terminal();
        for _ in 0..5 {
            t.process(b"$ \x1b[16G<r>\r\n");
        }
        t.process(b"$ \x1b[16G<r>\x1b[3G");
        t.set_predictive_echo("always");
        t.scroll_lines(1);
        assert!(t.predict_char('a'));
        t.scroll_to_bottom();
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "retired by its echo");
        let events = t.predict.events_for_test();
        assert_eq!(
            events.decorated_proofs, 0,
            "no history row compared: {events:?}"
        );
        assert_eq!(events.tail_refusals, 1, "{events:?}");
    }

    #[test]
    fn resize_drops_inflight_guesses() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        t.predict_char('a');
        t.resize(10, 40).expect("admitted geometry");
        assert!(t.predict_overlay().is_empty(), "stale-coords guess dropped");
        assert!(t.predict_next_deadline_ms().is_none());
    }
}
